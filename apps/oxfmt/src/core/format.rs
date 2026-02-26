#[cfg(feature = "napi")]
use std::borrow::Cow;
use std::path::Path;

use serde_json::Value;
#[cfg(feature = "napi")]
use tracing::debug;
use tracing::instrument;

use oxc_allocator::AllocatorPool;
use oxc_ast::ast::Statement;
use oxc_diagnostics::OxcDiagnostic;
use oxc_formatter::{
    AstNode, AstNodes, FormatOptions, FormatVueBindingParams, Formatter, QuoteStyle,
    enable_jsx_source_type, get_parse_options,
};
use oxc_parser::Parser;
use oxc_span::SourceType;

use super::{FormatFileStrategy, ResolvedOptions};

pub enum FormatResult {
    Success { is_changed: bool, code: String },
    Error(Vec<OxcDiagnostic>),
}

pub struct SourceFormatter {
    allocator_pool: AllocatorPool,
    #[cfg(feature = "napi")]
    external_formatter: Option<super::ExternalFormatter>,
}

impl SourceFormatter {
    pub fn new(num_of_threads: usize) -> Self {
        Self {
            allocator_pool: AllocatorPool::new(num_of_threads),
            #[cfg(feature = "napi")]
            external_formatter: None,
        }
    }

    #[cfg(feature = "napi")]
    #[must_use]
    pub fn with_external_formatter(
        mut self,
        external_formatter: Option<super::ExternalFormatter>,
    ) -> Self {
        self.external_formatter = external_formatter;
        self
    }

    /// Format a file based on its entry type and resolved options.
    #[instrument(level = "debug", name = "oxfmt::format", skip_all, fields(path = %entry.path().display()))]
    pub fn format(
        &self,
        entry: &FormatFileStrategy,
        source_text: &str,
        resolved_options: ResolvedOptions,
    ) -> FormatResult {
        let (result, insert_final_newline) = match (entry, resolved_options) {
            (
                FormatFileStrategy::OxcFormatter { path, source_type },
                ResolvedOptions::OxcFormatter {
                    format_options,
                    external_options,
                    filepath_override,
                    insert_final_newline,
                },
            ) => (
                self.format_by_oxc_formatter(
                    source_text,
                    path,
                    *source_type,
                    *format_options,
                    external_options,
                    filepath_override.as_deref(),
                ),
                insert_final_newline,
            ),
            (
                FormatFileStrategy::OxfmtToml { .. },
                ResolvedOptions::OxfmtToml { toml_options, insert_final_newline },
            ) => (Ok(Self::format_by_toml(source_text, toml_options)), insert_final_newline),
            #[cfg(feature = "napi")]
            (
                FormatFileStrategy::ExternalFormatter { path, parser_name },
                ResolvedOptions::ExternalFormatter {
                    external_options,
                    vue_internal,
                    vue_oxc_toolkit_spike,
                    insert_final_newline,
                },
            ) => (
                self.format_by_external_formatter(
                    source_text,
                    path,
                    parser_name,
                    external_options,
                    vue_internal,
                    vue_oxc_toolkit_spike,
                ),
                insert_final_newline,
            ),
            #[cfg(feature = "napi")]
            (
                FormatFileStrategy::ExternalFormatter { path, parser_name: "vue" },
                ResolvedOptions::OxcVueFormatter {
                    format_options,
                    external_options,
                    vue_oxc_toolkit_spike,
                    insert_final_newline,
                },
            ) => {
                let internal_result = self.format_by_oxc_vue_formatter(
                    source_text,
                    path,
                    *format_options,
                    &external_options,
                    vue_oxc_toolkit_spike,
                );
                (
                    internal_result.or_else(|err| {
                        debug!(
                            error = %err,
                            path = %path.display(),
                            "internal Vue formatter skeleton failed; falling back to external formatter"
                        );
                        self.format_by_external_formatter(
                            source_text,
                            path,
                            "vue",
                            external_options,
                            true,
                            vue_oxc_toolkit_spike,
                        )
                    }),
                    insert_final_newline,
                )
            }
            #[cfg(feature = "napi")]
            (
                FormatFileStrategy::ExternalFormatterPackageJson { path, parser_name },
                ResolvedOptions::ExternalFormatterPackageJson {
                    external_options,
                    sort_package_json,
                    insert_final_newline,
                },
            ) => (
                self.format_by_external_formatter_package_json(
                    source_text,
                    path,
                    parser_name,
                    external_options,
                    sort_package_json.as_ref(),
                ),
                insert_final_newline,
            ),
            _ => unreachable!("FormatFileStrategy and ResolvedOptions variant mismatch"),
        };

        match result {
            Ok(mut code) => {
                // NOTE: `insert_final_newline` relies on the fact that:
                // - each formatter already ensures there is traliling newline
                // - each formatter does not have an option to disable trailing newline
                // So we can trim it here without allocating new string.
                if !insert_final_newline {
                    let trimmed_len = code.trim_end().len();
                    code.truncate(trimmed_len);
                }

                FormatResult::Success { is_changed: source_text != code, code }
            }
            Err(err) => FormatResult::Error(vec![err]),
        }
    }

    /// Format JS/TS source code using oxc_formatter.
    #[cfg_attr(not(feature = "napi"), expect(unused_mut))]
    #[instrument(level = "debug", name = "oxfmt::format::oxc_formatter", skip_all)]
    fn format_by_oxc_formatter(
        &self,
        source_text: &str,
        path: &Path,
        source_type: SourceType,
        format_options: FormatOptions,
        mut external_options: Value,
        filepath_override: Option<&Path>,
    ) -> Result<String, OxcDiagnostic> {
        let source_type = enable_jsx_source_type(source_type);
        let allocator = self.allocator_pool.get();

        let ret = Parser::new(&allocator, source_text, source_type)
            .with_options(get_parse_options())
            .parse();
        if !ret.errors.is_empty() {
            // Return the first error for simplicity
            return Err(ret.errors.into_iter().next().unwrap());
        }

        #[cfg(feature = "napi")]
        let external_callbacks = {
            let external_formatter = self
                .external_formatter
                .as_ref()
                .expect("`external_formatter` must exist when `napi` feature is enabled");

            // Set `filepath` on options for Prettier plugins that depend on it,
            // and for the Tailwind sorter to resolve config.
            // `filepath_override` is `Some` in js-in-xxx flow (via `textToDoc()`),
            // where `path` is a dummy like `embedded.ts` but callbacks need the parent file path.
            // See `oxfmtrc::finalize_external_options()` for where this filepath originates.
            if let Value::Object(ref mut map) = external_options {
                let filepath = filepath_override.unwrap_or(path);
                map.insert(
                    "filepath".to_string(),
                    Value::String(filepath.to_string_lossy().to_string()),
                );
            }

            Some(external_formatter.to_external_callbacks(&format_options, external_options))
        };

        #[cfg(not(feature = "napi"))]
        let external_callbacks = {
            let _ = (path, external_options, filepath_override);
            None
        };

        let base_formatter = Formatter::new(&allocator, format_options);
        let formatted =
            base_formatter.format_with_external_callbacks(&ret.program, external_callbacks);

        let code = formatted.print().map_err(|err| {
            OxcDiagnostic::error(format!(
                "Failed to print formatted code: {}\n{err}",
                path.display()
            ))
        })?;

        #[cfg(feature = "detect_code_removal")]
        {
            if let Some(diff) =
                oxc_formatter::detect_code_removal(source_text, code.as_code(), source_type)
            {
                unreachable!("Code removal detected in `{}`:\n{diff}", path.to_string_lossy());
            }
        }

        Ok(code.into_code())
    }

    /// Format TOML file using `toml`.
    #[instrument(level = "debug", name = "oxfmt::format::oxc_toml", skip_all)]
    fn format_by_toml(source_text: &str, options: oxc_toml::Options) -> String {
        oxc_toml::format(source_text, options)
    }

    /// Format non-JS/TS file using external formatter (Prettier).
    #[cfg(feature = "napi")]
    #[instrument(level = "debug", name = "oxfmt::format::external_formatter", skip_all, fields(parser = %parser_name))]
    fn format_by_external_formatter(
        &self,
        source_text: &str,
        path: &Path,
        parser_name: &str,
        mut external_options: Value,
        _vue_internal: bool,
        _vue_oxc_toolkit_spike: bool,
    ) -> Result<String, OxcDiagnostic> {
        #[cfg(feature = "vue_oxc_toolkit_spike")]
        if parser_name == "vue"
            && (_vue_oxc_toolkit_spike || std::env::var_os("OXFMT_VUE_OXC_TOOLKIT_SPIKE").is_some())
        {
            let report = super::vue_oxc_toolkit_spike::parse_report(source_text);
            if report.panicked || report.error_count > 0 {
                debug!(
                    panicked = report.panicked,
                    error_count = report.error_count,
                    "vue_oxc_toolkit spike parse returned errors; continuing with external formatter"
                );
            }
        }

        let external_formatter = self
            .external_formatter
            .as_ref()
            .expect("`external_formatter` must exist when `napi` feature is enabled");

        // Set `parser` and `filepath` on options for Prettier.
        // We specify `parser` to skip parser inference for perf,
        // and `filepath` because some plugins depend on it.
        if let Value::Object(ref mut map) = external_options {
            map.insert("parser".to_string(), Value::String(parser_name.to_string()));
            map.insert("filepath".to_string(), Value::String(path.to_string_lossy().to_string()));
        }

        external_formatter.format_file(external_options, source_text).map_err(|err| {
            // NOTE: We are trying to make the error from oxc_formatter and external_formatter (Prettier) look similar.
            // Ideally, we would unify them into `OxcDiagnostic`,
            // which would eliminate the need for relative path conversion.
            // However, doing so would require:
            // - Parsing Prettier's error messages
            // - Converting span information from UTF-16 to UTF-8
            // This is a non-trivial amount of work, so for now, just leave this as a best effort.
            let relative = std::env::current_dir()
                .ok()
                .and_then(|cwd| path.strip_prefix(cwd).ok().map(Path::to_path_buf));
            let display_path = relative.as_deref().unwrap_or(path).to_string_lossy();
            let message = if let Some((first, rest)) = err.split_once('\n') {
                format!("{first}\n[{display_path}]\n{rest}")
            } else {
                format!("{err}\n[{display_path}]")
            };
            OxcDiagnostic::error(message)
        })
    }

    #[cfg(feature = "napi")]
    #[instrument(level = "debug", name = "oxfmt::format::oxc_vue_formatter", skip_all)]
    fn format_by_oxc_vue_formatter(
        &self,
        source_text: &str,
        path: &Path,
        format_options: FormatOptions,
        external_options: &Value,
        _vue_oxc_toolkit_spike: bool,
    ) -> Result<String, OxcDiagnostic> {
        #[cfg(feature = "vue_oxc_toolkit_spike")]
        if _vue_oxc_toolkit_spike || std::env::var_os("OXFMT_VUE_OXC_TOOLKIT_SPIKE").is_some() {
            let report = super::vue_oxc_toolkit_spike::parse_report(source_text);
            if report.panicked || report.error_count > 0 {
                return Err(OxcDiagnostic::error(format!(
                    "vue_oxc_toolkit spike parser failed (panicked={}, errors={})",
                    report.panicked, report.error_count
                )));
            }
        }

        if has_unsupported_multiline_directive_template_literal(source_text) {
            return Err(OxcDiagnostic::error(
                "internal Vue formatter does not support multiline template-literal directives yet",
            ));
        }
        if has_style_blocks(source_text) {
            return Err(OxcDiagnostic::error(
                "internal Vue formatter does not support style blocks yet",
            ));
        }
        if has_unsupported_event_binding_expressions(source_text) {
            return Err(OxcDiagnostic::error(
                "internal Vue formatter does not support complex event-binding expressions yet",
            ));
        }

        let script_blocks = super::vue_sfc::parse_script_blocks(source_text);

        let vue_indent_script_and_style = external_options
            .as_object()
            .and_then(|obj| obj.get("vueIndentScriptAndStyle"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let indent = if vue_indent_script_and_style {
            if format_options.indent_style.is_tab() {
                "\t".to_string()
            } else {
                " ".repeat(usize::from(format_options.indent_width.value()))
            }
        } else {
            String::new()
        };
        let line_ending = std::str::from_utf8(format_options.line_ending.as_bytes())
            .expect("line ending bytes should be valid utf-8");
        let template_indent = if format_options.indent_style.is_tab() {
            "\t".to_string()
        } else {
            " ".repeat(usize::from(format_options.indent_width.value()))
        };

        let mut output = source_text.to_string();
        for block in script_blocks.iter().rev() {
            let original = &source_text[block.content_start..block.content_end];
            if original.trim().is_empty() {
                continue;
            }

            let Some(source_type) = source_type_from_vue_script_lang(block.lang.as_deref()) else {
                continue;
            };
            let formatted = self.format_by_oxc_formatter(
                original,
                path,
                source_type,
                format_options.clone(),
                external_options.clone(),
                Some(path),
            )?;

            let replacement = wrap_formatted_vue_script(
                &formatted,
                line_ending,
                if vue_indent_script_and_style { Some(indent.as_str()) } else { None },
            );
            output.replace_range(block.content_start..block.content_end, &replacement);
        }

        let template_blocks = super::vue_sfc::parse_template_blocks(&output);
        for block in template_blocks.iter().rev() {
            let original = &output[block.content_start..block.content_end];
            let formatted = self.format_vue_template_block_mvp(
                original,
                line_ending,
                template_indent.as_str(),
                &format_options,
            );
            output.replace_range(block.content_start..block.content_end, &formatted);
        }

        Ok(trim_extra_trailing_line_endings(output, line_ending))
    }

    fn format_vue_template_block_mvp(
        &self,
        content: &str,
        line_ending: &str,
        indent: &str,
        format_options: &FormatOptions,
    ) -> String {
        let normalized = self.normalize_vue_directive_expressions(content, format_options);
        let normalized = self.normalize_vue_interpolations(&normalized, format_options);
        let normalized = normalize_template_literal_placeholders(&normalized);
        let normalized = normalize_unindented_template_lines(&normalized, indent, line_ending);
        let normalized = normalize_simple_text_elements(&normalized, line_ending);
        let normalized = normalize_single_root_template_layout(&normalized, line_ending, indent);
        trim_template_trailing_whitespace(&normalized, line_ending)
    }

    fn normalize_vue_interpolations(&self, input: &str, format_options: &FormatOptions) -> String {
        let mut output = String::with_capacity(input.len());
        let mut cursor = 0usize;

        while let Some(start_rel) = input[cursor..].find("{{") {
            let start = cursor + start_rel;
            output.push_str(&input[cursor..start]);
            output.push_str("{{");

            let expr_start = start + 2;
            let Some(end_rel) = input[expr_start..].find("}}") else {
                output.push_str(&input[expr_start..]);
                return output;
            };

            let expr_end = expr_start + end_rel;
            let expr = input[expr_start..expr_end].trim();
            let expr = self
                .format_vue_inline_expression(expr, format_options)
                .unwrap_or_else(|| expr.to_string());
            if !expr.is_empty() {
                output.push(' ');
                output.push_str(&expr);
                output.push(' ');
            }
            output.push_str("}}");

            cursor = expr_end + 2;
        }

        output.push_str(&input[cursor..]);
        output
    }

    fn normalize_vue_directive_expressions(
        &self,
        input: &str,
        format_options: &FormatOptions,
    ) -> String {
        let mut output = String::with_capacity(input.len());
        let mut cursor = 0usize;
        let bytes = input.as_bytes();
        let mut i = 0usize;

        while i + 1 < bytes.len() {
            if bytes[i] != b'=' || !matches!(bytes[i + 1], b'"' | b'\'') {
                i += 1;
                continue;
            }

            let quote = bytes[i + 1];
            let mut name_start = i;
            while name_start > 0 {
                let ch = bytes[name_start - 1] as char;
                if ch.is_whitespace() || matches!(ch, '<' | '/') {
                    break;
                }
                name_start -= 1;
            }
            let attr_name = &input[name_start..i];

            let value_start = i + 2;
            let mut value_end = value_start;
            while value_end < bytes.len() {
                if bytes[value_end] == quote
                    && (value_end == value_start || bytes[value_end - 1] != b'\\')
                {
                    break;
                }
                value_end += 1;
            }

            if value_end >= bytes.len() {
                break;
            }

            output.push_str(&input[cursor..value_start]);
            let value = &input[value_start..value_end];
            if attr_name == "v-for" {
                let trimmed = value.trim();
                let decoded = decode_vue_expression_entities(trimmed);
                let normalized = self
                    .format_vue_v_for_expression(&decoded, format_options)
                    .unwrap_or_else(|| value.to_string());
                output.push_str(&normalized);
            } else if should_format_vue_binding_attribute(attr_name) {
                let trimmed = value.trim();
                let decoded = decode_vue_expression_entities(trimmed);
                let normalized = self
                    .format_vue_binding_params(&decoded, format_options, false)
                    .unwrap_or_else(|| value.to_string());
                output.push_str(&normalized);
            } else if should_format_vue_directive_attribute(attr_name) {
                let trimmed = value.trim();
                let decoded = decode_vue_expression_entities(trimmed);
                let mut expression_options = format_options.clone();
                expression_options.quote_style =
                    if quote == b'"' { QuoteStyle::Single } else { QuoteStyle::Double };
                let normalized = self
                    .format_vue_inline_expression(&decoded, &expression_options)
                    .unwrap_or_else(|| value.to_string());
                output.push_str(&normalized);
            } else {
                output.push_str(value);
            }

            cursor = value_end;
            i = value_end + 1;
        }

        output.push_str(&input[cursor..]);
        output
    }

    fn format_vue_v_for_expression(
        &self,
        expression: &str,
        format_options: &FormatOptions,
    ) -> Option<String> {
        let (left, operator, right) = split_v_for_expression(expression)?;
        let left = strip_redundant_wrapping_parens(left.trim());
        let left = self.format_vue_binding_params(left, format_options, true)?;
        let right = self
            .format_vue_inline_expression(right.trim(), format_options)
            .unwrap_or_else(|| right.trim().to_string());

        Some(format!("{left} {operator} {right}"))
    }

    fn format_vue_binding_params(
        &self,
        binding: &str,
        format_options: &FormatOptions,
        is_v_for_binding_left: bool,
    ) -> Option<String> {
        if binding.is_empty() {
            return Some(String::new());
        }

        let wrapped = format!("function __oxfmt_vue_binding__({binding}) {{}}");
        let source_type =
            SourceType::from_extension("mjs").ok()?.with_module(true).with_standard(true);

        let allocator = self.allocator_pool.get();
        let ret = Parser::new(&allocator, &wrapped, source_type)
            .with_options(get_parse_options())
            .parse();
        if !ret.errors.is_empty() {
            return None;
        }

        let statement = ret.program.body.first()?;
        let Statement::FunctionDeclaration(func) = statement else {
            return None;
        };
        let params = &*func.params;
        let node = AstNode::new(params, AstNodes::Dummy(), &allocator);
        let include_parens =
            is_v_for_binding_left && (params.items.len() > 1 || params.rest.is_some());
        let content = FormatVueBindingParams::new(&node, include_parens);
        let formatted = Formatter::new(&allocator, format_options.clone()).format_node(
            &content,
            ret.program.source_text,
            source_type,
            &ret.program.comments,
            None,
        );
        let printed = formatted.print().ok()?.into_code();
        if binding.contains('\n') || binding.contains('\r') || binding.contains('`') {
            let result = printed.trim().to_string();
            if is_v_for_binding_left && include_parens {
                return Some(normalize_v_for_binding_left_layout(&result));
            }
            return Some(result);
        }
        let result = normalize_binding_params_layout(printed.trim());
        if is_v_for_binding_left && include_parens {
            return Some(normalize_v_for_binding_left_layout(&result));
        }
        Some(result)
    }

    fn format_vue_inline_expression(
        &self,
        expression: &str,
        format_options: &FormatOptions,
    ) -> Option<String> {
        if expression.is_empty() {
            return Some(String::new());
        }

        let wrapped = format!("const __oxfmt_vue_expr__ = {expression};");
        for extension in ["mjs", "ts"] {
            let source_type =
                SourceType::from_extension(extension).ok()?.with_module(true).with_standard(true);
            let allocator = self.allocator_pool.get();
            let ret = Parser::new(&allocator, &wrapped, source_type)
                .with_options(get_parse_options())
                .parse();
            if !ret.errors.is_empty() {
                continue;
            }

            let statement = ret.program.body.first()?;
            let Statement::VariableDeclaration(decl) = statement else {
                continue;
            };
            let declarator = decl.declarations.first()?;
            let Some(init) = declarator.init.as_ref() else {
                continue;
            };

            let printed = Formatter::new(&allocator, format_options.clone()).build(&ret.program);
            let rest = printed.trim().strip_prefix("const __oxfmt_vue_expr__ =")?.trim();
            let rest = rest.strip_suffix(';').unwrap_or(rest).trim();
            if matches!(init, oxc_ast::ast::Expression::AssignmentExpression(_)) {
                return Some(strip_redundant_wrapping_parens(rest).to_string());
            }
            return Some(rest.to_string());
        }

        None
    }

    /// Format `package.json`: optionally sort then format by external formatter.
    #[cfg(feature = "napi")]
    #[instrument(
        level = "debug",
        name = "oxfmt::format::external_formatter_package_json",
        skip_all
    )]
    fn format_by_external_formatter_package_json(
        &self,
        source_text: &str,
        path: &Path,
        parser_name: &str,
        external_options: Value,
        sort_options: Option<&sort_package_json::SortOptions>,
    ) -> Result<String, OxcDiagnostic> {
        let source_text: Cow<'_, str> = if let Some(options) = sort_options {
            match sort_package_json::sort_package_json_with_options(source_text, options) {
                Ok(sorted) => Cow::Owned(sorted),
                // `sort_package_json` can only handle strictly valid JSON.
                // On the other hand, Prettier's `json-stringify` parser is very permissive.
                // It can format JSON like input even with unquoted keys or trailing commas.
                // Therefore, rather than bailing out due to a sorting failure, we opt to format without sorting.
                Err(_) => Cow::Borrowed(source_text),
            }
        } else {
            Cow::Borrowed(source_text)
        };

        self.format_by_external_formatter(
            &source_text,
            path,
            parser_name,
            external_options,
            false,
            false,
        )
    }
}

fn source_type_from_vue_script_lang(lang: Option<&str>) -> Option<SourceType> {
    let extension = match lang {
        None => "mjs",
        Some("") => "mjs",
        Some("js" | "javascript") => "mjs",
        Some("ts" | "typescript") => "ts",
        Some("tsx") => "tsx",
        Some("jsx") => "jsx",
        Some(other) => other,
    };

    let Ok(mut source_type) = SourceType::from_extension(extension) else {
        return None;
    };

    // Vue script blocks are module-oriented by default.
    if source_type.is_unambiguous() {
        source_type = source_type.with_module(true);
    }

    // Keep non-JSX scripts in standard mode where applicable.
    if !extension.contains('x') {
        source_type = source_type.with_standard(true);
    }

    Some(source_type)
}

fn wrap_formatted_vue_script(formatted: &str, line_ending: &str, indent: Option<&str>) -> String {
    let trimmed = formatted.trim_end();
    if trimmed.is_empty() {
        return line_ending.to_string();
    }

    let mut output = String::new();
    output.push_str(line_ending);

    for line in trimmed.lines() {
        if let Some(indent) = indent
            && !line.is_empty()
        {
            output.push_str(indent);
        }
        output.push_str(line);
        output.push_str(line_ending);
    }

    output
}

fn normalize_template_literal_placeholders(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0usize;

    while let Some(start_rel) = input[cursor..].find("${") {
        let start = cursor + start_rel;
        output.push_str(&input[cursor..start]);
        output.push_str("${");

        let expr_start = start + 2;
        let Some(end_rel) = input[expr_start..].find('}') else {
            output.push_str(&input[expr_start..]);
            return output;
        };

        let expr_end = expr_start + end_rel;
        output.push_str(input[expr_start..expr_end].trim());
        output.push('}');
        cursor = expr_end + 1;
    }

    output.push_str(&input[cursor..]);
    output
}

fn normalize_unindented_template_lines(input: &str, indent: &str, line_ending: &str) -> String {
    if !(input.contains('\n') || input.contains('\r')) {
        return input.to_string();
    }

    let lines: Vec<&str> = input.lines().collect();
    let base_indent = lines
        .iter()
        .filter_map(|line| {
            let trimmed = line.trim_start_matches([' ', '\t']);
            if trimmed.is_empty() || trimmed.len() == line.len() {
                return None;
            }
            Some(&line[..line.len() - trimmed.len()])
        })
        .min_by_key(|value| value.len())
        .unwrap_or(indent);

    let mut output = String::new();
    for (idx, line) in lines.iter().enumerate() {
        if idx > 0 {
            output.push_str(line_ending);
        }

        let trimmed = line.trim_start_matches([' ', '\t']);
        if !trimmed.is_empty()
            && trimmed.len() == line.len()
            && (trimmed.starts_with('<') || trimmed.starts_with("{{"))
        {
            output.push_str(base_indent);
            output.push_str(trimmed);
        } else {
            output.push_str(line);
        }
    }

    if input.ends_with('\n') || input.ends_with('\r') {
        output.push_str(line_ending);
    }
    output
}

fn normalize_simple_text_elements(input: &str, line_ending: &str) -> String {
    let mut lines: Vec<String> = input.lines().map(ToString::to_string).collect();
    let mut idx = 0usize;

    while idx + 2 < lines.len() {
        let open_line = lines[idx].clone();
        let text_line = lines[idx + 1].clone();
        let close_line = lines[idx + 2].clone();

        let open_trim = open_line.trim();
        let text_trim = text_line.trim();
        let close_trim = close_line.trim();

        let open_indent_len = open_line.len() - open_line.trim_start_matches([' ', '\t']).len();
        let close_indent_len = close_line.len() - close_line.trim_start_matches([' ', '\t']).len();
        let same_indent = open_indent_len == close_indent_len;

        if same_indent
            && !text_trim.is_empty()
            && !text_trim.starts_with('<')
            && is_simple_open_tag(open_trim)
            && is_matching_close_tag(open_trim, close_trim)
        {
            let indent_prefix = &open_line[..open_indent_len];
            let collapsed = format!("{indent_prefix}{open_trim}{text_trim}{close_trim}");
            lines.splice(idx..=idx + 2, [collapsed]);
            continue;
        }
        idx += 1;
    }

    let mut output = lines.join(line_ending);
    if input.ends_with('\n') || input.ends_with('\r') {
        output.push_str(line_ending);
    }
    output
}

fn is_simple_open_tag(line: &str) -> bool {
    line.starts_with('<')
        && !line.starts_with("</")
        && !line.starts_with("<!")
        && !line.ends_with("/>")
        && line.ends_with('>')
}

fn is_matching_close_tag(open: &str, close: &str) -> bool {
    if !close.starts_with("</") || !close.ends_with('>') {
        return false;
    }
    let open_name = extract_tag_name(open.trim_start_matches('<'));
    let close_name = extract_tag_name(close.trim_start_matches("</"));
    matches!((open_name, close_name), (Some(open), Some(close)) if open == close)
}

fn extract_tag_name(value: &str) -> Option<&str> {
    let end =
        value.find(|ch: char| ch.is_whitespace() || ch == '>' || ch == '/').unwrap_or(value.len());
    if end == 0 { None } else { Some(&value[..end]) }
}

fn normalize_single_root_template_layout(input: &str, line_ending: &str, indent: &str) -> String {
    if input.contains('\n') || input.contains('\r') {
        return input.to_string();
    }

    let trimmed = input.trim();
    if !trimmed.starts_with('<') || !trimmed.ends_with('>') {
        return input.to_string();
    }

    let segments = split_compact_template_segments(trimmed);
    if segments.len() > 1 {
        let mut output = String::new();
        output.push_str(line_ending);

        let mut depth = 0usize;
        for segment in segments {
            let segment = segment.trim();
            if segment.is_empty() {
                continue;
            }

            if segment.starts_with("</") {
                depth = depth.saturating_sub(1);
            }

            for _ in 0..=depth {
                output.push_str(indent);
            }
            output.push_str(segment);
            output.push_str(line_ending);

            if is_simple_open_tag(segment)
                && !has_inline_open_close_pair(segment)
                && !is_void_tag_open_line(segment)
            {
                depth = depth.saturating_add(1);
            }
        }

        return output;
    }

    let mut output = String::new();
    output.push_str(line_ending);
    output.push_str(indent);
    output.push_str(trimmed);
    output.push_str(line_ending);
    output
}

fn split_compact_template_segments(input: &str) -> Vec<&str> {
    if !input.contains("><") {
        return vec![input];
    }

    let bytes = input.as_bytes();
    let mut in_quote: Option<u8> = None;
    let mut start = 0usize;
    let mut segments = Vec::new();
    let mut idx = 0usize;

    while idx < bytes.len() {
        let ch = bytes[idx];
        match ch {
            b'"' | b'\'' => {
                if in_quote == Some(ch) {
                    in_quote = None;
                } else if in_quote.is_none() {
                    in_quote = Some(ch);
                }
            }
            b'>' if in_quote.is_none() && idx + 1 < bytes.len() && bytes[idx + 1] == b'<' => {
                segments.push(&input[start..=idx]);
                start = idx + 1;
            }
            _ => {}
        }
        idx += 1;
    }

    if start < input.len() {
        segments.push(&input[start..]);
    }

    segments
}

fn has_inline_open_close_pair(trimmed: &str) -> bool {
    let Some(first_gt) = trimmed.find('>') else {
        return false;
    };
    let Some(open_tag_name) = extract_tag_name(trimmed.trim_start_matches('<')) else {
        return false;
    };
    let close_tag = format!("</{open_tag_name}>");
    trimmed[first_gt + 1..].contains(&close_tag)
}

fn is_void_tag_open_line(trimmed: &str) -> bool {
    const VOID_TAGS: [&str; 14] = [
        "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param",
        "source", "track", "wbr",
    ];

    let Some(tag) = extract_tag_name(trimmed.trim_start_matches('<')) else {
        return false;
    };

    VOID_TAGS.contains(&tag)
}

fn trim_template_trailing_whitespace(input: &str, line_ending: &str) -> String {
    let has_trailing_newline = input.ends_with('\n') || input.ends_with('\r');

    let mut output = String::new();
    for (idx, line) in input.lines().enumerate() {
        if idx > 0 {
            output.push_str(line_ending);
        }
        output.push_str(line.trim_end_matches([' ', '\t']));
    }

    if has_trailing_newline {
        output.push_str(line_ending);
    }

    output
}

fn trim_extra_trailing_line_endings(mut input: String, line_ending: &str) -> String {
    let double_line_ending = format!("{line_ending}{line_ending}");
    while input.ends_with(&double_line_ending) {
        let new_len = input.len().saturating_sub(line_ending.len());
        input.truncate(new_len);
    }
    input
}

fn should_format_vue_directive_attribute(attr_name: &str) -> bool {
    attr_name.starts_with(':')
        || attr_name.starts_with('.')
        || attr_name.starts_with('@')
        || attr_name.starts_with("v-bind:")
        || attr_name.starts_with("v-on:")
        || attr_name.starts_with("v-model")
        || matches!(
            attr_name,
            "v-if" | "v-else-if" | "v-show" | "v-html" | "v-text" | "v-for" | "v-slot"
        )
}

fn should_format_vue_binding_attribute(attr_name: &str) -> bool {
    attr_name == "v-slot" || attr_name.starts_with("v-slot:") || attr_name.starts_with('#')
}

fn decode_vue_expression_entities(value: &str) -> String {
    if !value.contains('&') {
        return value.to_string();
    }

    value
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn has_style_blocks(source_text: &str) -> bool {
    !super::vue_sfc::parse_style_blocks(source_text).is_empty()
}

fn has_unsupported_event_binding_expressions(source_text: &str) -> bool {
    let template_blocks = super::vue_sfc::parse_template_blocks(source_text);
    template_blocks.into_iter().any(|block| {
        let content = &source_text[block.content_start..block.content_end];
        let bytes = content.as_bytes();
        let mut i = 0usize;
        while i + 1 < bytes.len() {
            if bytes[i] != b'=' || !matches!(bytes[i + 1], b'"' | b'\'') {
                i += 1;
                continue;
            }

            let quote = bytes[i + 1];
            let mut name_start = i;
            while name_start > 0 {
                let ch = bytes[name_start - 1] as char;
                if ch.is_whitespace() || matches!(ch, '<' | '/') {
                    break;
                }
                name_start -= 1;
            }
            let attr_name = &content[name_start..i];

            let value_start = i + 2;
            let mut value_end = value_start;
            while value_end < bytes.len() {
                if bytes[value_end] == quote
                    && (value_end == value_start || bytes[value_end - 1] != b'\\')
                {
                    break;
                }
                value_end += 1;
            }
            if value_end >= bytes.len() {
                break;
            }

            if attr_name.starts_with('@') || attr_name.starts_with("v-on:") {
                let value = content[value_start..value_end].trim();
                if is_complex_event_binding_expression(value) {
                    return true;
                }
            }

            i = value_end + 1;
        }
        false
    })
}

fn is_complex_event_binding_expression(value: &str) -> bool {
    value.contains('\n')
        || value.contains('\r')
        || value.contains(';')
        || value.starts_with("if ")
        || value.starts_with("if(")
        || value.starts_with("function")
        || (value.contains("=>") && value.contains(':'))
}

fn has_unsupported_multiline_directive_template_literal(source_text: &str) -> bool {
    let template_blocks = super::vue_sfc::parse_template_blocks(source_text);
    template_blocks.into_iter().any(|block| {
        let content = &source_text[block.content_start..block.content_end];
        let bytes = content.as_bytes();
        let mut i = 0usize;
        while i + 1 < bytes.len() {
            if bytes[i] != b'=' || !matches!(bytes[i + 1], b'"' | b'\'') {
                i += 1;
                continue;
            }

            let quote = bytes[i + 1];
            let mut name_start = i;
            while name_start > 0 {
                let ch = bytes[name_start - 1] as char;
                if ch.is_whitespace() || matches!(ch, '<' | '/') {
                    break;
                }
                name_start -= 1;
            }
            let attr_name = &content[name_start..i];

            let value_start = i + 2;
            let mut value_end = value_start;
            while value_end < bytes.len() {
                if bytes[value_end] == quote
                    && (value_end == value_start || bytes[value_end - 1] != b'\\')
                {
                    break;
                }
                value_end += 1;
            }
            if value_end >= bytes.len() {
                break;
            }

            let is_directive_like = attr_name == "v-for"
                || should_format_vue_binding_attribute(attr_name)
                || should_format_vue_directive_attribute(attr_name);
            if is_directive_like {
                let value = &content[value_start..value_end];
                if value.contains('`') && (value.contains('\n') || value.contains('\r')) {
                    return true;
                }
            }

            i = value_end + 1;
        }
        false
    })
}

fn split_v_for_expression(expression: &str) -> Option<(&str, &str, &str)> {
    let mut paren_depth = 0usize;
    let mut brace_depth = 0usize;
    let mut bracket_depth = 0usize;
    let mut in_single = false;
    let mut in_double = false;
    let mut in_template = false;
    let mut escaped = false;
    let bytes = expression.as_bytes();
    let mut idx = 0usize;

    while idx < bytes.len() {
        let ch = bytes[idx] as char;

        if escaped {
            escaped = false;
            idx += 1;
            continue;
        }

        match ch {
            '\\' if in_single || in_double || in_template => escaped = true,
            '\'' if !in_double && !in_template => in_single = !in_single,
            '"' if !in_single && !in_template => in_double = !in_double,
            '`' if !in_single && !in_double => in_template = !in_template,
            '(' if !in_single && !in_double && !in_template => paren_depth += 1,
            ')' if !in_single && !in_double && !in_template && paren_depth > 0 => paren_depth -= 1,
            '[' if !in_single && !in_double && !in_template => bracket_depth += 1,
            ']' if !in_single && !in_double && !in_template && bracket_depth > 0 => {
                bracket_depth -= 1;
            }
            '{' if !in_single && !in_double => brace_depth += 1,
            '}' if !in_single && !in_double && brace_depth > 0 => brace_depth -= 1,
            _ => {}
        }

        if !(in_single || in_double || in_template)
            && paren_depth == 0
            && brace_depth == 0
            && bracket_depth == 0
        {
            if expression[idx..].starts_with(" in ") {
                return Some((&expression[..idx], "in", &expression[idx + 4..]));
            }
            if expression[idx..].starts_with(" of ") {
                return Some((&expression[..idx], "of", &expression[idx + 4..]));
            }
        }

        idx += 1;
    }

    None
}

fn normalize_binding_params_layout(input: &str) -> String {
    if !input.contains('\n') && !input.contains('\r') {
        return input.to_string();
    }

    let compact =
        input.lines().map(str::trim).filter(|line| !line.is_empty()).collect::<Vec<_>>().join(" ");
    compact.replace(", }", " }").replace(", ]", " ]")
}

fn normalize_v_for_binding_left_layout(input: &str) -> String {
    input.replace("( ", "(").replace(" )", ")")
}

fn strip_redundant_wrapping_parens(input: &str) -> &str {
    if !input.starts_with('(') || !input.ends_with(')') {
        return input;
    }

    let mut depth = 0usize;
    for (idx, ch) in input.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                if depth == 0 {
                    return input;
                }
                depth -= 1;
                if depth == 0 && idx != input.len() - 1 {
                    return input;
                }
            }
            _ => {}
        }
    }

    if depth == 0 { &input[1..input.len() - 1] } else { input }
}

#[cfg(test)]
mod tests {
    use oxc_formatter::{FormatOptions, QuoteStyle};

    use super::{SourceFormatter, source_type_from_vue_script_lang, wrap_formatted_vue_script};

    #[test]
    fn test_source_type_from_vue_script_lang() {
        let source_type = source_type_from_vue_script_lang(None).unwrap();
        assert!(source_type.is_module());

        let source_type = source_type_from_vue_script_lang(Some("")).unwrap();
        assert!(source_type.is_module());

        let source_type = source_type_from_vue_script_lang(Some("ts")).unwrap();
        assert!(source_type.is_typescript());
        assert!(source_type.is_module());

        let source_type = source_type_from_vue_script_lang(Some("tsx")).unwrap();
        assert!(source_type.is_typescript());
        assert!(source_type.is_jsx());
    }

    #[test]
    fn test_wrap_formatted_vue_script() {
        let code = "const a = 1;\nconst b = 2;\n";

        let wrapped = wrap_formatted_vue_script(code, "\n", None);
        assert_eq!(wrapped, "\nconst a = 1;\nconst b = 2;\n");

        let wrapped = wrap_formatted_vue_script(code, "\n", Some("  "));
        assert_eq!(wrapped, "\n  const a = 1;\n  const b = 2;\n");
    }

    #[test]
    fn test_format_vue_template_block_mvp() {
        let formatter = SourceFormatter::new(1);
        let source = "  <Comp :label=\"`${ foo }`\">{{msg}}</Comp>   \n  <p>{{  msg  }}</p>\n";
        let formatted =
            formatter.format_vue_template_block_mvp(source, "\n", "  ", &FormatOptions::default());
        assert_eq!(formatted, "  <Comp :label=\"`${foo}`\">{{ msg }}</Comp>\n  <p>{{ msg }}</p>\n");
    }

    #[test]
    fn test_format_vue_template_block_mvp_single_line_layout() {
        let formatter = SourceFormatter::new(1);
        let source = "   <div>{{answer}}</div> ";
        let formatted =
            formatter.format_vue_template_block_mvp(source, "\n", "  ", &FormatOptions::default());
        assert_eq!(formatted, "\n  <div>{{ answer }}</div>\n");
    }

    #[test]
    fn test_format_vue_template_block_mvp_single_line_siblings_layout() {
        let formatter = SourceFormatter::new(1);
        let source = "<p>foo</p><div>foo</div>";
        let formatted =
            formatter.format_vue_template_block_mvp(source, "\n", "  ", &FormatOptions::default());
        assert_eq!(formatted, "\n  <p>foo</p>\n  <div>foo</div>\n");
    }

    #[test]
    fn test_format_vue_template_block_mvp_single_line_nested_layout() {
        let formatter = SourceFormatter::new(1);
        let source = "<div><p>foo</p><div>bar</div></div>";
        let formatted =
            formatter.format_vue_template_block_mvp(source, "\n", "  ", &FormatOptions::default());
        assert_eq!(formatted, "\n  <div>\n    <p>foo</p>\n    <div>bar</div>\n  </div>\n");
    }

    #[test]
    fn test_format_vue_inline_expression() {
        let formatter = SourceFormatter::new(1);
        let expression =
            formatter.format_vue_inline_expression("a+b", &FormatOptions::default()).unwrap();
        assert_eq!(expression, "a + b");
    }

    #[test]
    fn test_format_vue_inline_expression_with_typescript_syntax() {
        let formatter = SourceFormatter::new(1);
        let expression = formatter
            .format_vue_inline_expression("value as Foo satisfies Bar", &FormatOptions::default())
            .unwrap();
        assert_eq!(expression, "value as Foo satisfies Bar");
    }

    #[test]
    fn test_format_vue_inline_expression_with_single_quote_style() {
        let formatter = SourceFormatter::new(1);
        let mut options = FormatOptions::default();
        options.quote_style = QuoteStyle::Single;
        let expression = formatter.format_vue_inline_expression("\"list-\"+id", &options).unwrap();
        assert_eq!(expression, "'list-' + id");
    }

    #[test]
    fn test_format_vue_inline_assignment_expression() {
        let formatter = SourceFormatter::new(1);
        let expression =
            formatter.format_vue_inline_expression("count+=1", &FormatOptions::default()).unwrap();
        assert_eq!(expression, "count += 1");
    }

    #[test]
    fn test_format_vue_binding_params() {
        let formatter = SourceFormatter::new(1);
        let params = formatter
            .format_vue_binding_params("{foo=1,bar}", &FormatOptions::default(), false)
            .unwrap();
        assert_eq!(params, "{ foo = 1, bar }");
    }

    #[test]
    fn test_format_vue_v_for_expression() {
        let formatter = SourceFormatter::new(1);
        let expression = formatter
            .format_vue_v_for_expression("(item,index) in items", &FormatOptions::default())
            .unwrap();
        assert_eq!(expression, "(item, index) in items");
    }

    #[test]
    fn test_detects_unsupported_multiline_directive_template_literal() {
        let source = r#"
<template>
  <Comp #default="{ a = `line
${foo}` }">{{ a }}</Comp>
</template>
"#;
        assert!(super::has_unsupported_multiline_directive_template_literal(source));
    }

    #[test]
    fn test_allows_simple_directive_template_literal() {
        let source = r#"<template><Comp :label="`${foo}`">{{ foo }}</Comp></template>"#;
        assert!(!super::has_unsupported_multiline_directive_template_literal(source));
    }

    #[test]
    fn test_detects_style_blocks() {
        let source = r#"
<template><div/></template>
<style>.a { color: red; }</style>
"#;
        assert!(super::has_style_blocks(source));
    }

    #[test]
    fn test_normalize_unindented_template_content_lines() {
        let source = r#"
<template>
<span>{{(a||          b)}} {{z&&(a&&b)}}</span>
</template>
"#;
        let normalized = super::normalize_unindented_template_lines(source, "  ", "\n");
        assert!(normalized.contains("\n  <span>{{(a||          b)}} {{z&&(a&&b)}}</span>\n"));
    }

    #[test]
    fn test_normalize_simple_text_elements() {
        let source = "  <div>\n    hello\n  </div>\n";
        let normalized = super::normalize_simple_text_elements(source, "\n");
        assert_eq!(normalized, "  <div>hello</div>\n");
    }

    #[test]
    fn test_trim_extra_trailing_line_endings() {
        let input = "<template></template>\n\n".to_string();
        let trimmed = super::trim_extra_trailing_line_endings(input, "\n");
        assert_eq!(trimmed, "<template></template>\n");
    }

    #[test]
    fn test_decode_vue_expression_entities() {
        let value = "&quot;list-&quot; + &apos;x&apos; + &lt;tag&gt; + &amp;foo";
        let decoded = super::decode_vue_expression_entities(value);
        assert_eq!(decoded, "\"list-\" + 'x' + <tag> + &foo");
    }

    #[test]
    fn test_formats_dot_directive_attributes() {
        assert!(super::should_format_vue_directive_attribute(".disabled"));
    }

    #[test]
    fn test_detects_unsupported_event_binding_expressions() {
        let source = r#"
<template>
  <div @click="if (x === 1 as number) { log('hello') } else { log('nonhello') };" />
</template>
"#;
        assert!(super::has_unsupported_event_binding_expressions(source));
    }

    #[test]
    fn test_allows_simple_event_binding_expression() {
        let source = r#"<template><button @click="count += 1">{{ count }}</button></template>"#;
        assert!(!super::has_unsupported_event_binding_expressions(source));
    }
}

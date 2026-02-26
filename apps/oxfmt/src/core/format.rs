#[cfg(feature = "napi")]
use std::borrow::Cow;
use std::path::Path;

use serde_json::Value;
#[cfg(feature = "napi")]
use tracing::debug;
use tracing::instrument;

use oxc_allocator::AllocatorPool;
use oxc_diagnostics::OxcDiagnostic;
use oxc_formatter::{FormatOptions, Formatter, enable_jsx_source_type, get_parse_options};
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

        let script_blocks = super::vue_sfc::parse_script_blocks(source_text);
        if script_blocks.is_empty() {
            return Ok(source_text.to_string());
        }

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
            let formatted = format_vue_template_block_mvp(original, line_ending);
            output.replace_range(block.content_start..block.content_end, &formatted);
        }

        Ok(output)
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

fn format_vue_template_block_mvp(content: &str, line_ending: &str) -> String {
    let normalized = normalize_vue_interpolations(content);
    trim_template_trailing_whitespace(&normalized, line_ending)
}

fn normalize_vue_interpolations(input: &str) -> String {
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
        if !expr.is_empty() {
            output.push(' ');
            output.push_str(expr);
            output.push(' ');
        }
        output.push_str("}}");

        cursor = expr_end + 2;
    }

    output.push_str(&input[cursor..]);
    output
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

#[cfg(test)]
mod tests {
    use super::{
        format_vue_template_block_mvp, source_type_from_vue_script_lang, wrap_formatted_vue_script,
    };

    #[test]
    fn test_source_type_from_vue_script_lang() {
        let source_type = source_type_from_vue_script_lang(None).unwrap();
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
        let source = "  <div>{{a+b}}</div>   \n  <p>{{  msg  }}</p>\n";
        let formatted = format_vue_template_block_mvp(source, "\n");
        assert_eq!(formatted, "  <div>{{ a+b }}</div>\n  <p>{{ msg }}</p>\n");
    }
}

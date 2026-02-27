use oxc_allocator_registry::Allocator;
use vue_oxc_toolkit::VueOxcParser;

#[derive(Debug, Clone, Copy)]
pub(super) struct ParseReport {
    pub panicked: bool,
    pub error_count: usize,
}

/// Run the Vue parser spike and report parser stability metrics.
pub(super) fn parse_report(source_text: &str) -> ParseReport {
    let allocator = Allocator::default();
    let ret = VueOxcParser::new(&allocator, source_text).parse();
    ParseReport { panicked: ret.panicked, error_count: ret.errors.len() }
}

#[cfg(test)]
mod tests {
    use super::parse_report;

    #[test]
    fn parses_basic_vue_sfc() {
        let source = r#"
<template>
  <div>{{ msg }}</div>
</template>

<script setup lang="ts">
const msg = "hello";
</script>
"#;
        let report = parse_report(source);
        assert!(!report.panicked);
        assert_eq!(report.error_count, 0);
    }
}

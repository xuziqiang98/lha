use adam_agent::INTERACTIVE_SESSION_SOURCES;
use adam_app_server_protocol::ThreadSourceKind;
use adam_protocol::protocol::SessionSource as CoreSessionSource;

pub(crate) fn compute_source_filters(
    source_kinds: Option<Vec<ThreadSourceKind>>,
) -> (Vec<CoreSessionSource>, Option<Vec<ThreadSourceKind>>) {
    let Some(source_kinds) = source_kinds else {
        return (INTERACTIVE_SESSION_SOURCES.to_vec(), None);
    };

    if source_kinds.is_empty() {
        return (INTERACTIVE_SESSION_SOURCES.to_vec(), None);
    }

    let requires_post_filter = source_kinds.iter().any(|kind| {
        matches!(
            kind,
            ThreadSourceKind::Exec | ThreadSourceKind::AppServer | ThreadSourceKind::Unknown
        )
    });

    if requires_post_filter {
        (Vec::new(), Some(source_kinds))
    } else {
        let interactive_sources = source_kinds
            .iter()
            .filter_map(|kind| match kind {
                ThreadSourceKind::Cli => Some(CoreSessionSource::Cli),
                ThreadSourceKind::VsCode => Some(CoreSessionSource::VSCode),
                ThreadSourceKind::Exec
                | ThreadSourceKind::AppServer
                | ThreadSourceKind::Unknown => None,
            })
            .collect::<Vec<_>>();
        (interactive_sources, Some(source_kinds))
    }
}

pub(crate) fn source_kind_matches(source: &CoreSessionSource, filter: &[ThreadSourceKind]) -> bool {
    filter.iter().any(|kind| match kind {
        ThreadSourceKind::Cli => matches!(source, CoreSessionSource::Cli),
        ThreadSourceKind::VsCode => matches!(source, CoreSessionSource::VSCode),
        ThreadSourceKind::Exec => matches!(source, CoreSessionSource::Exec),
        ThreadSourceKind::AppServer => matches!(source, CoreSessionSource::Mcp),
        ThreadSourceKind::Unknown => matches!(source, CoreSessionSource::Unknown),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn compute_source_filters_defaults_to_interactive_sources() {
        let (allowed_sources, filter) = compute_source_filters(None);

        assert_eq!(allowed_sources, INTERACTIVE_SESSION_SOURCES.to_vec());
        assert_eq!(filter, None);
    }

    #[test]
    fn compute_source_filters_empty_means_interactive_sources() {
        let (allowed_sources, filter) = compute_source_filters(Some(Vec::new()));

        assert_eq!(allowed_sources, INTERACTIVE_SESSION_SOURCES.to_vec());
        assert_eq!(filter, None);
    }

    #[test]
    fn compute_source_filters_interactive_only_skips_post_filtering() {
        let source_kinds = vec![ThreadSourceKind::Cli, ThreadSourceKind::VsCode];
        let (allowed_sources, filter) = compute_source_filters(Some(source_kinds.clone()));

        assert_eq!(
            allowed_sources,
            vec![CoreSessionSource::Cli, CoreSessionSource::VSCode]
        );
        assert_eq!(filter, Some(source_kinds));
    }
}

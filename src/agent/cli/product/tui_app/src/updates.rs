#![cfg(any(not(debug_assertions), test))]

use crate::product::agent::config::Config;
use crate::product::agent::default_client::create_client;
use crate::product::tui_app::app_event::AppEvent;
use crate::product::tui_app::app_event_sender::AppEventSender;
use crate::product::tui_app::history_cell::UpdateAvailableHistoryCell;
use chrono::DateTime;
use chrono::Duration;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;
use std::path::Path;
use std::path::PathBuf;

use crate::product::tui_app::version::CODEX_CLI_VERSION;

pub(crate) fn spawn_update_check(config: Config, app_event_tx: AppEventSender) {
    if !config.check_for_update_on_startup {
        return;
    }

    let version_file = version_filepath(&config);
    let info = read_version_info(&version_file).ok();
    let latest_notified = notify_if_newer(info.as_ref(), &app_event_tx);

    if match &info {
        None => true,
        Some(info) => info.last_checked_at < Utc::now() - Duration::hours(20),
    } {
        // Refresh the cached latest version without blocking TUI startup.
        // If the fresh result is newer, insert the same update card in this session.
        tokio::spawn(async move {
            match check_for_update(&version_file).await {
                Ok(info) => {
                    if latest_notified.as_deref() != Some(info.latest_version.as_str()) {
                        notify_if_newer(Some(&info), &app_event_tx);
                    }
                }
                Err(err) => {
                    tracing::error!("Failed to check for LHA update: {err}");
                }
            }
        });
    }
}

fn notify_if_newer(info: Option<&VersionInfo>, app_event_tx: &AppEventSender) -> Option<String> {
    let info = info?;
    if should_show_update_notice(info) {
        let latest_version = info.latest_version.clone();
        app_event_tx.send(AppEvent::InsertHistoryCell(Box::new(
            UpdateAvailableHistoryCell::new(
                latest_version.clone(),
                crate::product::tui_app::update_action::get_update_action(),
            ),
        )));
        Some(latest_version)
    } else {
        None
    }
}

fn should_show_update_notice(info: &VersionInfo) -> bool {
    is_newer(&info.latest_version, CODEX_CLI_VERSION).unwrap_or(false)
        && info.dismissed_version.as_deref() != Some(info.latest_version.as_str())
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct VersionInfo {
    latest_version: String,
    // ISO-8601 timestamp (RFC3339)
    last_checked_at: DateTime<Utc>,
    #[serde(default)]
    dismissed_version: Option<String>,
}

const VERSION_FILENAME: &str = "version.json";
const LATEST_RELEASE_URL: &str = "https://api.github.com/repos/xuziqiang98/lha/releases/latest";

#[derive(Deserialize, Debug, Clone)]
struct ReleaseInfo {
    tag_name: String,
}

fn version_filepath(config: &Config) -> PathBuf {
    config.lha_home.join(VERSION_FILENAME)
}

fn read_version_info(version_file: &Path) -> anyhow::Result<VersionInfo> {
    let contents = std::fs::read_to_string(version_file)?;
    Ok(serde_json::from_str(&contents)?)
}

async fn check_for_update(version_file: &Path) -> anyhow::Result<VersionInfo> {
    let ReleaseInfo {
        tag_name: latest_tag_name,
    } = create_client()
        .get(LATEST_RELEASE_URL)
        .send()
        .await?
        .error_for_status()?
        .json::<ReleaseInfo>()
        .await?;
    let latest_version = extract_version_from_latest_tag(&latest_tag_name)?;

    // Preserve any previously dismissed version if present.
    let prev_info = read_version_info(version_file).ok();
    let info = VersionInfo {
        latest_version,
        last_checked_at: Utc::now(),
        dismissed_version: prev_info.and_then(|p| p.dismissed_version),
    };

    if let Err(err) = write_version_info(version_file, &info).await {
        tracing::error!("Failed to write update cache: {err}");
    }
    Ok(info)
}

fn is_newer(latest: &str, current: &str) -> Option<bool> {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => Some(l > c),
        _ => None,
    }
}

async fn write_version_info(version_file: &Path, info: &VersionInfo) -> anyhow::Result<()> {
    let json_line = format!("{}\n", serde_json::to_string(info)?);
    if let Some(parent) = version_file.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(version_file, json_line).await?;
    Ok(())
}

fn extract_version_from_latest_tag(latest_tag_name: &str) -> anyhow::Result<String> {
    let version = latest_tag_name
        .trim()
        .strip_prefix('v')
        .unwrap_or_else(|| latest_tag_name.trim());
    if parse_version(version).is_some() {
        Ok(version.to_string())
    } else {
        Err(anyhow::anyhow!(
            "Failed to parse latest tag name '{latest_tag_name}'"
        ))
    }
}

/// Returns the latest version to show in a popup, if it should be shown.
/// This respects the user's dismissal choice for the current latest version.
pub fn get_upgrade_version_for_popup(config: &Config) -> Option<String> {
    if !config.check_for_update_on_startup {
        return None;
    }

    let version_file = version_filepath(config);
    let info = read_version_info(&version_file).ok()?;
    if should_show_update_notice(&info) {
        Some(info.latest_version)
    } else {
        None
    }
}

/// Persist a dismissal for the current latest version so we don't show
/// the update popup again for this version.
pub async fn dismiss_version(config: &Config, version: &str) -> anyhow::Result<()> {
    let version_file = version_filepath(config);
    let mut info = match read_version_info(&version_file) {
        Ok(info) => info,
        Err(_) => return Ok(()),
    };
    info.dismissed_version = Some(version.to_string());
    write_version_info(&version_file, &info).await
}

fn parse_version(v: &str) -> Option<(u64, u64, u64)> {
    let mut iter = v.trim().split('.');
    let maj = iter.next()?.parse::<u64>().ok()?;
    let min = iter.next()?.parse::<u64>().ok()?;
    let pat = iter.next()?.parse::<u64>().ok()?;
    if iter.next().is_some() {
        return None;
    }
    Some((maj, min, pat))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::product::agent::config::ConfigBuilder;
    use crate::product::tui_app::app_event::AppEvent;
    use crate::product::tui_app::app_event_sender::AppEventSender;
    use pretty_assertions::assert_eq;
    use tempfile::tempdir;
    use tokio::sync::mpsc::unbounded_channel;
    use toml::Value as TomlValue;

    #[test]
    fn extracts_plain_semver_release_tag() {
        assert_eq!(
            extract_version_from_latest_tag("1.2.3").expect("failed to parse version"),
            "1.2.3"
        );
    }

    #[test]
    fn extracts_v_prefixed_release_tag() {
        assert_eq!(
            extract_version_from_latest_tag("v1.2.3").expect("failed to parse version"),
            "1.2.3"
        );
    }

    #[test]
    fn rejects_non_semver_release_tag() {
        assert!(extract_version_from_latest_tag("rust-v1.2.3").is_err());
        assert!(extract_version_from_latest_tag("nightly").is_err());
    }

    #[test]
    fn prerelease_version_is_not_considered_newer() {
        assert_eq!(is_newer("0.11.0-beta.1", "0.11.0"), None);
        assert_eq!(is_newer("1.0.0-rc.1", "1.0.0"), None);
    }

    #[test]
    fn plain_semver_comparisons_work() {
        assert_eq!(is_newer("0.11.1", "0.11.0"), Some(true));
        assert_eq!(is_newer("0.11.0", "0.11.1"), Some(false));
        assert_eq!(is_newer("1.0.0", "0.9.9"), Some(true));
        assert_eq!(is_newer("0.9.9", "1.0.0"), Some(false));
        assert_eq!(is_newer("1.0.4-beta.1", "1.0.3"), None);
    }

    #[test]
    fn whitespace_is_ignored() {
        assert_eq!(parse_version(" 1.2.3 \n"), Some((1, 2, 3)));
        assert_eq!(is_newer(" 1.2.3 ", "1.2.2"), Some(true));
    }

    #[tokio::test]
    async fn cached_newer_version_emits_update_history_cell() {
        let lha_home = tempdir().expect("tempdir");
        let config = ConfigBuilder::default()
            .lha_home(lha_home.path().to_path_buf())
            .build()
            .await
            .expect("config");

        let info = VersionInfo {
            latest_version: "999.0.0".to_string(),
            last_checked_at: Utc::now(),
            dismissed_version: None,
        };
        let version_file = lha_home.path().join(VERSION_FILENAME);
        std::fs::write(
            &version_file,
            format!(
                "{}\n",
                serde_json::to_string(&info).expect("serialize version info")
            ),
        )
        .expect("write version file");

        let (tx, mut rx) = unbounded_channel();
        spawn_update_check(config, AppEventSender::new(tx));

        assert!(matches!(rx.try_recv(), Ok(AppEvent::InsertHistoryCell(_))));
    }

    #[tokio::test]
    async fn cached_dismissed_newer_version_emits_no_update_history_cell() {
        let lha_home = tempdir().expect("tempdir");
        let config = ConfigBuilder::default()
            .lha_home(lha_home.path().to_path_buf())
            .build()
            .await
            .expect("config");

        let info = VersionInfo {
            latest_version: "999.0.0".to_string(),
            last_checked_at: Utc::now(),
            dismissed_version: Some("999.0.0".to_string()),
        };
        let version_file = lha_home.path().join(VERSION_FILENAME);
        std::fs::write(
            &version_file,
            format!(
                "{}\n",
                serde_json::to_string(&info).expect("serialize version info")
            ),
        )
        .expect("write version file");

        let (tx, mut rx) = unbounded_channel();
        spawn_update_check(config, AppEventSender::new(tx));

        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn newer_version_after_dismissed_version_emits_update_history_cell() {
        let lha_home = tempdir().expect("tempdir");
        let config = ConfigBuilder::default()
            .lha_home(lha_home.path().to_path_buf())
            .build()
            .await
            .expect("config");

        let info = VersionInfo {
            latest_version: "999.0.0".to_string(),
            last_checked_at: Utc::now(),
            dismissed_version: Some("998.0.0".to_string()),
        };
        let version_file = lha_home.path().join(VERSION_FILENAME);
        std::fs::write(
            &version_file,
            format!(
                "{}\n",
                serde_json::to_string(&info).expect("serialize version info")
            ),
        )
        .expect("write version file");

        let (tx, mut rx) = unbounded_channel();
        spawn_update_check(config, AppEventSender::new(tx));

        assert!(matches!(rx.try_recv(), Ok(AppEvent::InsertHistoryCell(_))));
    }

    #[tokio::test]
    async fn disabled_update_check_emits_no_event() {
        let lha_home = tempdir().expect("tempdir");
        let config = ConfigBuilder::default()
            .lha_home(lha_home.path().to_path_buf())
            .cli_overrides(vec![(
                "check_for_update_on_startup".to_string(),
                TomlValue::Boolean(false),
            )])
            .build()
            .await
            .expect("config");

        let (tx, mut rx) = unbounded_channel();
        spawn_update_check(config, AppEventSender::new(tx));

        assert!(rx.try_recv().is_err());
    }
}

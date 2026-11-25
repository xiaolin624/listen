//! `--no-tray` (issue #55): the notification-area icon and its transient bubbles
//! can be turned off per launch without changing the default. Both answers are
//! pure launch configuration, so the whole contract is asserted at that seam;
//! `gui` only reads them when it creates the icon.

use listen::launch::{Config, Engine, LaunchAction, Recognition};
use std::path::Path;

fn config(arguments: &[&str]) -> Config {
    match Config::parse(
        Path::new(r"C:\listen-issue-55"),
        arguments.iter().map(|argument| (*argument).to_string()),
    ) {
        Ok(LaunchAction::Run(config)) => config,
        other => panic!("expected one run configuration for {arguments:?}: {other:?}"),
    }
}

#[test]
fn the_notification_area_icon_is_installed_by_default_and_absent_with_no_tray() {
    assert!(
        config(&[]).tray(),
        "the default launch still installs the notification-area icon"
    );
    assert!(
        !config(&["--no-tray"]).tray(),
        "--no-tray must not install a notification-area icon"
    );
}

#[test]
fn no_tray_suppresses_transient_bubbles_and_hidden_still_does() {
    assert!(
        config(&[]).notify(),
        "the default launch still shows phase bubbles"
    );
    assert!(
        !config(&["--no-tray"]).notify(),
        "without a tray icon there is nowhere to show a bubble"
    );
    assert!(
        !config(&["--hidden"]).notify(),
        "--hidden already suppresses transient notifications"
    );
    assert!(!config(&["--no-tray", "--hidden"]).notify());
}

#[test]
fn no_tray_changes_no_other_launch_decision() {
    let base = config(&[
        "--engine",
        "sensevoice",
        "--runtime",
        r"C:\listen-issue-55\runtime",
    ]);
    let no_tray = config(&[
        "--engine",
        "sensevoice",
        "--runtime",
        r"C:\listen-issue-55\runtime",
        "--no-tray",
    ]);
    assert_eq!(no_tray.recognition(), base.recognition());
    assert_eq!(
        no_tray.recognition(),
        Recognition::Local(Engine::SenseVoice),
        "the flag must not disturb the recognition decision"
    );
    assert_eq!(no_tray.runtime(), base.runtime());
    assert_eq!(no_tray.hidden(), base.hidden());
    assert_eq!(no_tray.glossary(), base.glossary());
    // A standalone switch, not a value-taking option: parsing continues with the
    // next argument and a glued spelling stays an unknown option.
    assert_eq!(
        config(&["--no-tray", "--engine", "sensevoice"]).recognition(),
        Recognition::Local(Engine::SenseVoice)
    );
    assert!(matches!(
        Config::parse(
            Path::new(r"C:\listen-issue-55"),
            ["--no-tray=1".to_string(), "x".to_string()]
        ),
        Err(message) if message.contains("Unknown option")
    ));
}

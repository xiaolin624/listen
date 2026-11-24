//! The one opt-in test that talks to the real DeepSeek API.
//!
//! It is ignored by default so the normal suite is fully offline. Run it explicitly
//! with a key in the local environment:
//!
//! ```powershell
//! $env:DEEPSEEK_API_KEY = 'sk-...'
//! & E:\luyin\.artifacts\run-cargo.ps1 -Task Raw -Manifest E:\luyin\worktrees\issue-21\app\listen\Cargo.toml `
//!     -Extra 'test --test refine_deepseek_api -- --ignored --nocapture'
//! ```
//!
//! Without the key it prints a notice and passes, so what was and was not proven is
//! explicit instead of a silent skip. The fake-port suite in `refine_backfill.rs` is
//! the automated evidence for the safety boundary; this test only proves that the
//! real request shape reaches the real service and comes back parsed.

#![cfg(windows)]

use listen::{RefineText, deepseek::DeepSeek};

#[test]
#[ignore = "requires a real DEEPSEEK_API_KEY and network access; run it explicitly"]
fn the_real_deepseek_api_corrects_one_transcript() {
    let configured = std::env::var("DEEPSEEK_API_KEY")
        .map(|key| !key.trim().is_empty())
        .unwrap_or(false);
    if !configured {
        println!("DEEPSEEK_API_KEY is not set; skipping the real DeepSeek refine test.");
        return;
    }
    let client = DeepSeek::from_environment();
    client
        .availability()
        .expect("a non-empty DEEPSEEK_API_KEY is configured");

    // A Mandarin sentence with the kind of garbled English technical term 精修 exists
    // for. Only this string is sent.
    let transcript = "我们使用 deep sick harnessness 来做测试，延迟 preformance 还可以。";
    let refined = client
        .refine(transcript)
        .expect("the real DeepSeek API should return a correction");
    println!("transcript: {transcript}");
    println!("refined:    {refined}");

    assert!(
        !refined.trim().is_empty(),
        "the correction must not be empty"
    );
    assert!(
        refined.to_lowercase().contains("deepseek"),
        "the obvious proper-noun error should be corrected: {refined}"
    );
    assert!(
        refined.contains("来做测试"),
        "the Chinese content must be preserved, not translated: {refined}"
    );
}

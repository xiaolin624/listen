#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod capsule;
mod capsule_window;
mod gui;
mod panel;
mod panel_window;
mod status;

use listen::launch::{self, LaunchAction};
use std::env;

const HELP: &str = "Listen: Ctrl+Alt+R starts/stops capture of the default multimedia render endpoint.
Only that endpoint is captured; audio routed only to the default communication endpoint or to another device is outside the capture. A default-route switch or endpoint removal interrupts the capture, retains received audio/checkpoint, and marks the result incomplete.

Default recognition: the accepted Alibaba cloud trial. Each capture's continuous audio and capture-start glossary go to the Beijing Message model (Alibaba Model Studio / qwen-audio-3.1-asr-flash-message / cn-beijing). Every start must pass the live default-workspace, exact-model free-only protection check and the shared CNY 50 ledger before any audio is sent; a key or billing alert does not bypass this protection. If the credential, workspace, model or region is missing or invalid, the capture is refused with the reason and does not recognise locally instead. Received audio is retained.

Local recognition (SenseVoice q8 through sherpa-onnx, resident in this process on the CPU) is an explicit fallback: select it with --engine sensevoice (or --engine whisper / a whisper-cli.exe path). It is never started silently in place of a failed cloud request, and the UI/status names the path actually used.

Options:
  --cloud-trial <ali>
        explicitly select the default Alibaba Model Studio /
        qwen-audio-3.1-asr-flash-message / cn-beijing path. Configure locally
        with scripts/configure-cloud-trial.ps1 (hidden key entry; current-user
        Windows Credential Manager). LISTEN_ALI_API_KEY and
        LISTEN_ALI_WORKSPACE_ID are optional process-local test injections.
        Every paid attempt, including retries, shares the one persisted CNY 50
        first-trial ledger; the panel reports this tool's cumulative and monthly
        usage, and the CNY 100/month line is a reminder, never a hard stop.
        Cannot be combined with --engine; there is no automatic cloud/local fallback.
  --engine <sensevoice|whisper|whisper-cli.exe>
        Select local recognition explicitly. sensevoice selects the resident local
        engine; whisper or a path selects the whisper.cpp fallback. An explicit
        engine deselects the cloud default and cannot be combined with
        --cloud-trial.
  --sherpa-lib <directory>
        directory holding sherpa-onnx-c-api.dll and onnxruntime.dll.
  --sensevoice-model <model.int8.onnx>
        pinned SenseVoice q8 model.
  --sensevoice-tokens <tokens.txt>
        SenseVoice tokens.
  --sensevoice-vad <silero_vad.onnx>
        pinned Silero VAD model used as the non-speech gate.
  --sensevoice-language <auto|zh|en|...>
        SenseVoice language hint (default auto).
  --model <ggml.bin> --vad-model <ggml-silero.bin> --language <zh|auto|...> --backend <host|cli>
        whisper.cpp fallback settings; --backend host (default) keeps the model resident, cli starts one process per window.
  --runtime <directory> --threads <count> --hidden --no-tray
        runtime files, recognition threads and start-hidden behavior. --no-tray
        starts without a notification-area icon; the capsule's right-click menu
        is then the exit entry. Without the flag the tray icon is installed as
        before.
  --pending-audio-budget-mib <MiB>
        pending capture audio allowed across every segment before a new capture is
        refused and a running one is stopped (default 1024 = 1 GiB). The measurement
        counts every segment's temporary upload, overlapping recognition windows and
        repair/retry files, not just the final WAV. Saved text has its own bound.
  --pending-audio-warn-mib <MiB>
        pending audio at which the approaching-limit warning is shown; the warning
        never stops anything (default: four fifths of the budget).
  --free-disk-floor-mib <MiB>
        free space that must remain on the runtime volume; at or below it no new
        capture starts and a running one is stopped with its received audio kept
        (default 1024 = 1 GiB). This is a storage condition, never a network wait.
  --glossary <glossary.txt>
        local UTF-8 term list, one term per line, re-read at every capture start
        (default: <runtime>\\glossary.txt, then glossary.txt next to listen.exe;
        the dev launcher passes packaging\\glossary.txt). A malformed file is
        reported and the uncorrected transcript is still delivered.

Control running application: --control <start|stop|toggle|retry|cleanup|show|panel|copy|refine|quit|delete-segment:<n>>
Retry recognizes the same audio retained after a failed local recognition; cleanup deletes that audio.
delete-segment:<n> deletes exactly one segment's pending recording and pending result (the
capture order shown in the panel). Every other segment and unfinished task is untouched;
a segment that is still recovering is stopped safely first, and late results cannot
recreate it or write to a target.
show opens the expanded result panel, panel toggles it and copy copies the complete current result to the clipboard.
refine asks the DeepSeek API to correct the complete current result. Only that transcript text is sent, never the audio.
The refined text replaces the appended transcript only while the original target still holds exactly what Listen wrote; if the target was edited, the refined text stays in the panel and the edit is preserved. Without DEEPSEEK_API_KEY in the local environment the refine entry is unavailable, and local recognition is unaffected.
Clicking the status capsule toggles the expanded result panel; the panel shows the complete current result and the status detail. Text is never submitted as a message automatically.";

fn main() {
    // A control invocation is part of a script or another automation. It must
    // never block on a modal dialog: report the failure on stderr and use the exit
    // code. Interactive launch errors keep the existing visible message.
    let control_mode = env::args().any(|argument| argument == "--control");
    let result = match launch::root() {
        Ok(root) => launch::Config::parse(&root, env::args().skip(1)),
        Err(error) => Err(error),
    };
    let result = match result {
        Ok(LaunchAction::Run(config)) => {
            // The window owns Application construction, so hand it the explicit term
            // list (if any) before it starts. The file itself is re-read at every
            // capture; without --glossary the application resolves its own default.
            if let Some(glossary) = config.glossary() {
                listen::glossary::set_default_path(glossary.to_path_buf());
            }
            gui::run(config)
        }
        Ok(LaunchAction::Control(control)) => gui::control(&control),
        Ok(LaunchAction::Help) => {
            gui::show_help(HELP);
            Ok(())
        }
        Err(error) => {
            if control_mode {
                control_error(&error);
                std::process::exit(2);
            }
            gui::show_error(&error);
            std::process::exit(2);
        }
    };
    if let Err(error) = result {
        if control_mode {
            control_error(&error);
            std::process::exit(1);
        }
        gui::show_error(&error);
        std::process::exit(1);
    }
}

/// Writes a control failure without assuming a console or a valid stderr handle.
fn control_error(message: &str) {
    use std::io::Write;
    let _ = writeln!(std::io::stderr(), "Listen control failed: {message}");
}

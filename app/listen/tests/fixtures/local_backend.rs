// Deterministic external executable for Application flow tests, speaking the
// whisper.cpp CLI's file/JSON protocol (including split UTF-8 token bytes).
use std::{env, fs, io::Write, path::PathBuf};

fn main() {
    let args: Vec<_> = env::args().collect();
    if args.iter().any(|s| s == "--help") { return; }
    if args.iter().any(|s| s == "-vm") {
        println!("Detected 1 speech segments:\nSpeech segment 0: start = 0.0, end = 0.1");
        return;
    }
    let value = |key: &str| args[args.iter().position(|a| a == key).unwrap() + 1].clone();
    let path = PathBuf::from(value("-f"));
    let wav = fs::read(path).unwrap();
    let first_sample = i16::from_le_bytes([wav[44], wav[45]]);
    let stem = PathBuf::from(value("-of"));
    let (text, tokens): (&str, Vec<(&[u8], u64, u64)>) = match first_sample {
        1000 => ("开始跨段", vec![(&[0xe5, 0xbc], 0, 20), (&[0x80], 20, 40), ("始".as_bytes(), 40, 80), ("跨段".as_bytes(), 80, 100)]),
        2000 => ("跨段 Rust 收尾。", vec![("跨段".as_bytes(), 0, 30), (b" Rust", 30, 90), (" 收尾。".as_bytes(), 90, 100)]),
        3000 => (" Rust 收尾。", vec![(b" Rust", 0, 10), (" 收尾。".as_bytes(), 10, 40)]),
        _ => panic!("unexpected deterministic playback sample"),
    };
    fs::write(stem.with_extension("txt"), text).unwrap();
    if !args.iter().any(|a| a == "-ojf") { return; }
    let mut json = fs::File::create(stem.with_extension("json")).unwrap();
    write!(json, "{{\"model\":{{\"multilingual\":true}},\"transcription\":[{{\"text\":\"{text}\",\"offsets\":{{\"from\":0,\"to\":100}},\"tokens\":[{{\"text\":\"[_BEG_]\",\"id\":50364}}").unwrap();
    for (bytes, from, to) in tokens {
        write!(json, ",{{\"text\":\"").unwrap();
        json.write_all(bytes).unwrap();
        write!(json, "\",\"id\":123,\"offsets\":{{\"from\":{from},\"to\":{to}}}}}").unwrap();
    }
    write!(json, "]}}]}}").unwrap();
}

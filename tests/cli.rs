#![cfg(unix)]

use std::{env, fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Command};

struct Fixture(PathBuf);

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// Dobles de FFmpeg: fuerzan tamaños y errores que no dependen del hardware del runner.
#[test]
fn cli_retries_enforces_limit_and_requires_gpu() {
    let root = Fixture(env::temp_dir().join(format!("video-cli-test-{}", std::process::id())));
    fs::create_dir(&root.0).unwrap();
    for mode in ["retry", "oversize", "no-gpu"] {
        let dir = root.0.join(mode);
        fs::create_dir(&dir).unwrap();
        for (name, script) in [
            ("ffprobe", "#!/bin/sh\ncase \"$*\" in *format=duration*) echo 20;; *) printf 'video\\naudio\\n';; esac\n"),
            ("ffmpeg", r#"#!/bin/sh
if [ "$FAKE_MODE" = no-gpu ]; then echo 'GPU unavailable' >&2; exit 1; fi
for last do :; done
if [ "$last" = - ]; then exit 0; fi
count=0
if [ -f "$FAKE_STATE/count" ]; then read -r count < "$FAKE_STATE/count"; fi
count=$((count + 1))
echo "$count" > "$FAKE_STATE/count"
size=9000000
if [ "$FAKE_MODE" = retry ] && [ "$count" -gt 1 ]; then size=7000000; fi
truncate -s "$size" "$last"
"#),
        ] {
            let path = dir.join(name);
            fs::write(&path, script).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let input = dir.join("original con espacios.mp4");
        let output = dir.join("salida.mp4");
        fs::write(&input, b"original").unwrap();
        let mut paths = vec![dir.clone()];
        paths.extend(env::split_paths(&env::var_os("PATH").unwrap_or_default()));
        let invoke = || {
            Command::new(env!("CARGO_BIN_EXE_video-to-8mb"))
                .arg(&input)
                .args(["--preset", "8mb", "-o"])
                .arg(&output)
                .env("PATH", env::join_paths(&paths).unwrap())
                .env("FAKE_MODE", mode)
                .env("FAKE_STATE", &dir)
                .output()
                .unwrap()
        };
        let result = invoke();
        match mode {
            "retry" => {
                assert!(
                    result.status.success(),
                    "{}",
                    String::from_utf8_lossy(&result.stderr)
                );
                assert_eq!(fs::read_to_string(dir.join("count")).unwrap().trim(), "2");
                assert_eq!(fs::metadata(&output).unwrap().len(), 7_000_000);
                assert!(!invoke().status.success());
                assert_eq!(fs::metadata(&output).unwrap().len(), 7_000_000);
            }
            "oversize" => {
                assert!(!result.status.success());
                assert!(!output.exists());
                assert_eq!(fs::read_to_string(dir.join("count")).unwrap().trim(), "4");
            }
            _ => {
                assert!(!result.status.success());
                assert!(!output.exists());
                assert!(String::from_utf8_lossy(&result.stderr)
                    .contains("No hay un codificador GPU operativo"));
            }
        }
        assert_eq!(fs::read(&input).unwrap(), b"original");
        assert!(!fs::read_dir(&dir).unwrap().any(|p| p
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".video-to-8mb-")));
    }
}

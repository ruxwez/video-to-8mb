use std::{
    env,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    process::{self, Command, Stdio},
};

type Result<T> = std::result::Result<T, String>;
const HELP: &str = "video-to-8mb <vídeo> [--preset 8mb|10mb|100mb] [-o salida.mp4]

Comprime con GPU a un MP4 H.264/AAC bajo el límite elegido (8mb por defecto).
MB = 1.000.000 bytes. Conserva el vídeo completo; nunca sobrescribe archivos.
Requiere ffmpeg, ffprobe y una GPU con su controlador y codificador compatibles.
GPU automática: NVIDIA NVENC, Intel QSV, AMD AMF, VAAPI o Apple VideoToolbox.
";

#[derive(Debug)]
struct Options {
    input: PathBuf,
    output: PathBuf,
    mb: u64,
}

fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Option<Options>> {
    let mut args = args.into_iter();
    let (mut input, mut output, mut mb) = (None, None, 8);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("-h" | "--help") => return Ok(None),
            Some("--preset") => {
                mb = match args.next().as_deref().and_then(|v| v.to_str()) {
                    Some("8mb") => 8,
                    Some("10mb") => 10,
                    Some("100mb") => 100,
                    _ => return Err("Usa --preset 8mb, 10mb o 100mb.".into()),
                };
            }
            Some("-o" | "--output") => {
                output = Some(PathBuf::from(
                    args.next().ok_or("Falta la ruta de salida.")?,
                ));
            }
            Some("--") => {
                for arg in args.by_ref() {
                    if input.replace(PathBuf::from(arg)).is_some() {
                        return Err("Solo se admite un vídeo de entrada.".into());
                    }
                }
            }
            Some(value) if value.starts_with('-') => {
                return Err(format!("Opción desconocida: {value}"));
            }
            _ => {
                if input.replace(PathBuf::from(arg)).is_some() {
                    return Err("Solo se admite un vídeo de entrada.".into());
                }
            }
        }
    }
    let input: PathBuf = input.ok_or("Falta el vídeo de entrada. Usa --help para ver el uso.")?;
    let output = output.unwrap_or_else(|| {
        let mut name = input.file_stem().unwrap_or_default().to_os_string();
        name.push(format!("-{mb}mb.mp4"));
        input.with_file_name(name)
    });
    if !output
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("mp4"))
    {
        return Err("La salida debe tener extensión .mp4.".into());
    }
    Ok(Some(Options { input, output, mb }))
}

fn probe(input: &Path, entries: &str) -> Result<String> {
    let result = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            entries,
            "-of",
            "default=nw=1:nk=1",
        ])
        .arg(input)
        .output()
        .map_err(|e| format!("No se pudo ejecutar ffprobe (¿está instalado?): {e}"))?;
    if !result.status.success() {
        return Err(format!(
            "ffprobe: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&result.stdout).trim().into())
}

fn budget(mb: u64, duration: f64, audio: bool) -> Result<(u64, u64)> {
    if !duration.is_finite() || duration <= 0.0 {
        return Err("El vídeo no tiene una duración finita válida.".into());
    }
    // Reserva espacio para el contenedor y las variaciones del codificador.
    let total = (mb as f64 * 1_000_000.0 * 8.0 * 0.94 / duration) as u64;
    let audio_rate = if audio {
        (total / 5).clamp(16_000, 128_000)
    } else {
        0
    };
    let video_rate = total.saturating_sub(audio_rate).min(100_000_000);
    if video_rate < 32_000 {
        return Err("Vídeo demasiado largo para este límite; elige un preset mayor.".into());
    }
    Ok((video_rate, audio_rate))
}

#[derive(Debug)]
struct Encoder {
    name: &'static str,
    device: Option<PathBuf>,
}

impl Encoder {
    fn setup(&self, command: &mut Command) {
        if let Some(device) = &self.device {
            command.arg("-vaapi_device").arg(device);
        }
    }

    fn video(&self, command: &mut Command) {
        let scale = "scale=trunc(iw/2)*2:trunc(ih/2)*2";
        let filter = if self.name == "h264_vaapi" {
            format!("{scale},format=nv12,hwupload")
        } else {
            format!("{scale},format=yuv420p")
        };
        command.args(["-vf", &filter, "-c:v", self.name]);
        if self.name == "h264_videotoolbox" {
            command.args(["-allow_sw", "0"]);
        }
        match self.name {
            "h264_nvenc" => {
                command.args(["-rc", "vbr"]);
            }
            "h264_amf" => {
                command.args(["-rc", "vbr_peak"]);
            }
            "h264_vaapi" => {
                command.args(["-rc_mode", "VBR"]);
            }
            _ => {}
        }
    }
}

fn detect_encoder() -> Result<Encoder> {
    let mut candidates = vec![
        Encoder {
            name: "h264_nvenc",
            device: None,
        },
        Encoder {
            name: "h264_qsv",
            device: None,
        },
        Encoder {
            name: "h264_amf",
            device: None,
        },
    ];
    if let Ok(devices) = fs::read_dir("/dev/dri") {
        let mut devices: Vec<_> = devices
            .flatten()
            .map(|d| d.path())
            .filter(|p| {
                p.file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with("renderD"))
            })
            .collect();
        devices.sort();
        candidates.extend(devices.into_iter().map(|device| Encoder {
            name: "h264_vaapi",
            device: Some(device),
        }));
    }
    candidates.push(Encoder {
        name: "h264_videotoolbox",
        device: None,
    });
    let mut failures = Vec::new();
    for encoder in candidates {
        let mut command = Command::new("ffmpeg");
        command.args(["-hide_banner", "-loglevel", "error", "-nostdin"]);
        encoder.setup(&mut command);
        command.args(["-f", "lavfi", "-i", "color=size=128x128:rate=30"]);
        encoder.video(&mut command);
        let result = command
            .args(["-b:v", "1000000", "-frames:v", "1", "-f", "null", "-"])
            .output()
            .map_err(|e| format!("No se pudo ejecutar ffmpeg (¿está instalado?): {e}"))?;
        if result.status.success() {
            return Ok(encoder);
        }
        failures.push(format!(
            "{}: {}",
            encoder.name,
            String::from_utf8_lossy(&result.stderr).trim()
        ));
    }
    Err(format!("No hay un codificador GPU operativo. Comprueba el controlador, el acceso a la GPU y la compilación de FFmpeg.\n{}", failures.join("\n")))
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(parent: &Path) -> Result<Self> {
        for index in 0..100 {
            let path = parent.join(format!(".video-to-8mb-{}-{index}", process::id()));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(format!("No se puede crear la salida temporal: {e}")),
            }
        }
        Err("No se pudo crear un directorio temporal libre.".into())
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn encode(input: &Path, output: &Path, encoder: &Encoder, video: u64, audio: u64) -> Result<()> {
    let mut command = Command::new("ffmpeg");
    command.args(["-hide_banner", "-loglevel", "warning", "-nostdin", "-y"]);
    encoder.setup(&mut command);
    command.arg("-i").arg(input).args([
        "-map",
        "0:v:0",
        "-map_metadata",
        "-1",
        "-map_chapters",
        "-1",
    ]);
    encoder.video(&mut command);
    command
        .arg("-b:v")
        .arg(video.to_string())
        .arg("-maxrate")
        .arg(video.to_string())
        .arg("-bufsize")
        .arg((video * 2).to_string());
    if audio > 0 {
        command
            .args(["-map", "0:a:0", "-c:a", "aac", "-ac", "2", "-b:a"])
            .arg(audio.to_string());
    } else {
        command.arg("-an");
    }
    let status = command
        .args(["-sn", "-dn", "-movflags", "+faststart", "-f", "mp4"])
        .arg(output)
        .stdin(Stdio::null())
        .status()
        .map_err(|e| format!("No se pudo ejecutar ffmpeg: {e}"))?;
    if !status.success() {
        return Err("FFmpeg no pudo comprimir el vídeo; consulta el error anterior.".into());
    }
    Ok(())
}

fn compress(options: &Options, encoder: Option<&Encoder>) -> Result<u64> {
    let input = fs::canonicalize(&options.input).map_err(|e| format!("Entrada no válida: {e}"))?;
    if !input.is_file() {
        return Err("La entrada debe ser un archivo de vídeo.".into());
    }
    if fs::symlink_metadata(&options.output).is_ok() {
        return Err(
            "La salida ya existe; elige otro nombre. No se sobrescribió ningún archivo.".into(),
        );
    }
    let duration: f64 = probe(&input, "format=duration")?
        .parse()
        .map_err(|_| "No se pudo determinar la duración del vídeo.")?;
    let streams = probe(&input, "stream=codec_type")?;
    if !streams.lines().any(|s| s == "video") {
        return Err("El archivo no contiene vídeo.".into());
    }
    let (mut video, mut audio) =
        budget(options.mb, duration, streams.lines().any(|s| s == "audio"))?;
    let detected;
    let encoder = match encoder {
        Some(encoder) => encoder,
        None => {
            detected = detect_encoder()?;
            &detected
        }
    };
    let parent = options
        .output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let scratch = Scratch::new(parent)?;
    let temporary = scratch.0.join("output.mp4");
    let limit = options.mb * 1_000_000;
    for attempt in 1..=4 {
        eprintln!(
            "{} · {} MB · intento {attempt}/4 · vídeo {} kb/s",
            encoder.name,
            options.mb,
            video / 1000
        );
        encode(&input, &temporary, encoder, video, audio)?;
        let bytes = fs::metadata(&temporary).map_err(|e| e.to_string())?.len();
        if bytes == 0 {
            return Err("FFmpeg produjo un archivo vacío.".into());
        }
        if bytes <= limit {
            // Publica sin sobrescribir, incluso si otro proceso crea la salida mientras se codifica.
            fs::hard_link(&temporary, &options.output)
                .map_err(|e| format!("No se pudo guardar la salida sin sobrescribir: {e}"))?;
            return Ok(bytes);
        }
        let ratio = limit as f64 / bytes as f64 * 0.90;
        video = (video as f64 * ratio) as u64;
        audio = (audio as f64 * ratio) as u64;
        if video < 32_000 || (audio > 0 && audio < 16_000) {
            break;
        }
    }
    Err("No se pudo cumplir el límite conservando el vídeo completo. Prueba un preset mayor; no se guardó la salida.".into())
}

fn run() -> Result<()> {
    let Some(options) = parse(env::args_os().skip(1))? else {
        print!("{HELP}");
        return Ok(());
    };
    let bytes = compress(&options, None)?;
    println!(
        "{} · {:.2} MB (límite {} MB)",
        options.output.display(),
        bytes as f64 / 1_000_000.0,
        options.mb
    );
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("Error: {error}");
        process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(args: &[&str]) -> Result<Option<Options>> {
        parse(args.iter().map(OsString::from))
    }

    #[test]
    fn presets_and_paths() {
        for mb in [8, 10, 100] {
            let preset = format!("{mb}mb");
            let parsed = options(&["mi vídeo.mov", "--preset", &preset])
                .unwrap()
                .unwrap();
            assert_eq!(parsed.mb, mb);
            assert_eq!(
                parsed.output,
                PathBuf::from(format!("mi vídeo-{preset}.mp4"))
            );
        }
        assert_eq!(
            options(&["--", "-video.mov"]).unwrap().unwrap().input,
            PathBuf::from("-video.mov")
        );
        assert!(options(&["--help"]).unwrap().is_none());
    }

    #[test]
    fn invalid_arguments() {
        for args in [
            vec![],
            vec!["a", "b"],
            vec!["a", "--preset", "9mb"],
            vec!["a", "--preset"],
            vec!["a", "-o"],
            vec!["a", "-o", "out.mkv"],
            vec!["a", "--unknown"],
        ] {
            assert!(options(&args).is_err(), "{args:?}");
        }
    }

    #[test]
    fn bitrate_includes_audio_and_margin() {
        for mb in [8, 10, 100] {
            let (video, audio) = budget(mb, 120.0, true).unwrap();
            assert!(audio >= 16_000);
            assert!((video + audio) as f64 * 120.0 / 8.0 <= mb as f64 * 1_000_000.0 * 0.94);
            assert_eq!(budget(mb, 120.0, false).unwrap().1, 0);
        }
        for duration in [0.0, -1.0, f64::NAN, f64::INFINITY, 1e10] {
            assert!(budget(8, duration, true).is_err());
        }
    }

    #[test]
    #[ignore = "requiere ffmpeg/ffprobe con libx264; valida el flujo sin GPU"]
    fn real_ffmpeg_pipeline() {
        let root = Scratch::new(&env::temp_dir()).unwrap();
        for with_audio in [true, false] {
            let input = root
                .0
                .join(format!("entrada con espacios-{with_audio}.mp4"));
            let mut command = Command::new("ffmpeg");
            command.args([
                "-v",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=320x180:rate=24",
            ]);
            if with_audio {
                command.args(["-f", "lavfi", "-i", "sine=frequency=440"]);
            }
            assert!(command
                .args(["-t", "3", "-c:v", "libx264", "-threads", "1", "-c:a", "aac"])
                .arg(&input)
                .status()
                .unwrap()
                .success());
            for mb in [8, 10, 100] {
                let output = root.0.join(format!("salida-{with_audio}-{mb}.mp4"));
                let options = Options {
                    input: input.clone(),
                    output: output.clone(),
                    mb,
                };
                let encoder = Encoder {
                    name: "libx264",
                    device: None,
                };
                let bytes = compress(&options, Some(&encoder)).unwrap();
                assert!(bytes > 0 && bytes <= mb * 1_000_000);
                let duration: f64 = probe(&output, "format=duration").unwrap().parse().unwrap();
                assert!((duration - 3.0).abs() < 0.15);
                let streams = probe(&output, "stream=codec_type").unwrap();
                assert_eq!(streams.lines().any(|s| s == "audio"), with_audio);
                assert!(compress(&options, Some(&encoder))
                    .unwrap_err()
                    .contains("ya existe"));
                assert_eq!(fs::metadata(&output).unwrap().len(), bytes);
            }
        }
    }
}

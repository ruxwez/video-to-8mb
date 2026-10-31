# video-to-8mb

CLI en Rust para comprimir un vídeo a **un máximo de 8, 10 o 100 MB**, con
codificación H.264 acelerada por GPU y audio AAC en un archivo MP4.
Sin dependencias de Rust: utiliza `ffmpeg` y `ffprobe` como programas externos.

## Requisitos

- Rust estable y Cargo para compilar.
- `ffmpeg` y `ffprobe` en el `PATH`.
- GPU y controlador compatibles, y FFmpeg compilado con su codificador:
  NVIDIA NVENC, Intel Quick Sync (QSV), AMD AMF, VAAPI en Linux o
  VideoToolbox en macOS. En Linux se necesita acceso al dispositivo GPU
  (`/dev/dri/renderD*` para VAAPI).

La detección prueba una codificación real con cada backend en ese orden.
Si ninguno funciona, termina mostrando los errores; **no utiliza CPU como
alternativa automática**. La decodificación, el escalado y el audio se procesan
en CPU; la GPU codifica el vídeo.

## Uso

```sh
cargo build --release

./target/release/video-to-8mb video.mov
./target/release/video-to-8mb video.mov --preset 10mb
./target/release/video-to-8mb "mi vídeo.mkv" --preset 100mb -o comprimido.mp4
./target/release/video-to-8mb --help
```

En Windows, el ejecutable es `target\release\video-to-8mb.exe`.
Para instalarlo en el `PATH`: `cargo install --path .`.

Por defecto usa `8mb` y guarda `video-8mb.mp4` junto al original. Nunca
sobrescribe el original ni una salida existente; usa `-o` para cambiar el nombre.
La carpeta de destino debe existir y permitir enlaces duros (por ejemplo,
ext4, APFS o NTFS), utilizados para guardar sin sobrescribir otros archivos.

## Cómo ajusta el tamaño

MB significa **1.000.000 bytes**, no MiB. Calcula el bitrate según la duración,
reserva espacio para audio y contenedor, y comprueba el tamaño real al terminar.
Si supera el límite, reduce el bitrate y vuelve a codificar desde el original,
hasta cuatro intentos. Solo publica la salida cuando cumple el límite.
No corta el vídeo para cumplirlo ni añade relleno: puede ocupar menos del máximo.

Conserva la primera pista de vídeo y, si existe, la primera de audio (en estéreo).
No incluye subtítulos, otras pistas, capítulos ni metadatos del contenedor.
Mantiene la resolución, redondeando las dimensiones impares a números pares.
Para vídeos muy largos y límites pequeños, la calidad puede bajar mucho o el
programa puede pedir un preset mayor. Un fallo o una cancelación no sustituye
archivos existentes; una interrupción forzada puede dejar una carpeta temporal
`.video-to-8mb-*` en el destino que se puede borrar.

## Pruebas

```sh
cargo test
cargo test -- --ignored --nocapture
cargo clippy --all-targets -- -D warnings
```

Las pruebas normales comprueban argumentos, presets y presupuesto de bitrate.
También simulan resultados de FFmpeg para verificar los reintentos, el rechazo
de salidas demasiado grandes y el error cuando no hay GPU.
La prueba marcada como ignorada requiere FFmpeg con `libx264`: genera vídeos
con y sin audio, ejecuta los tres presets y comprueba tamaño, duración y
protección contra sobrescrituras. Usa CPU **solo dentro de esa prueba** para
validar el flujo en equipos sin GPU. Para comprobar el backend GPU, ejecuta el
CLI con un vídeo en el equipo donde se vaya a utilizar.

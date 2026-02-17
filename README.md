# plexus-listen

Plexus plugin for audio capture from wireless microphone systems (DJI MIC MINI and others). Streams, records, meters, and monitors live audio over RPC.

## Architecture

Each RPC method owns its resources (cpal streams, ring buffers, resampler) inside the stream block. Everything is cleaned up automatically when the client disconnects — no shared state, no leaks.

The monitor path uses `rubato::Fft` for FFT-based resampling when input and output sample rates differ (e.g. 48kHz capture → 44.1kHz output).

## Build & Run

```bash
cd plexus-listen
cargo build
cargo run -- -p 4447
```

## Methods

All methods are accessed via synapse through the `listen` backend and `mic` namespace:

```bash
synapse -P 4447 listen mic <method> [--param value ...]
```

| Method | Params | Description |
|--------|--------|-------------|
| `info` | — | USB device info via rusb |
| `list_devices` | — | Enumerate audio input devices |
| `list_channels` | `--device`, `--labels` | Enumerate channels with labels |
| `record` | `--path`, `--device` | Record to WAV file |
| `levels` | `--device` | Stream RMS/peak level meters |
| `stream_audio` | `--device`, `--chunk_ms` | Stream interleaved base64 PCM chunks |
| `stream_channels` | `--device`, `--channels`, `--labels`, `--chunk_ms` | Stream per-channel mono audio + levels |
| `probe` | `--device`, `--duration_ms` | Per-channel signal analysis |
| `monitor` | `--device` | Live playback through server output |
| `status` | — | Device connectivity check |

The `--device` param is optional on all methods that open audio — omit it to auto-detect the DJI MIC MINI, or pass a device name substring to select a specific input.

## Streaming Audio Playback

`stream_audio` emits base64-encoded f32le PCM chunks over RPC. To play them locally in real time, pipe synapse output through `pcm-play`:

```bash
synapse -P 4447 listen mic stream_audio | ./pcm-play
```

For lower latency, use smaller chunks:

```bash
synapse -P 4447 listen mic stream_audio --chunk_ms 20 | ./pcm-play
```

`pcm-play` accepts optional arguments for sample rate and channel count (defaults: 48000 Hz, stereo):

```bash
synapse -P 4447 listen mic stream_audio | ./pcm-play 48000 2
```

### How it works

The `stream_audio` template (`~/.config/synapse/templates/mic/stream_audio.mustache`) is configured to output one raw base64 string per audio chunk. `pcm-play` decodes each line from base64 to raw PCM bytes and pipes them into ffplay with buffering disabled for minimal latency.

### Requirements

- `ffplay` (from ffmpeg)
- `python3` (for base64 decoding)

### Generate templates

If the synapse templates aren't installed yet:

```bash
synapse -P 4447 -g listen mic
```

## Recording

Record to a WAV file — stops and finalizes when the client disconnects (Ctrl+C):

```bash
synapse -P 4447 listen mic record --path /tmp/recording.wav
```

## Monitoring

Live playback through the server's audio output device. Useful when running locally:

```bash
synapse -P 4447 listen mic monitor
```

## Level Metering

Stream real-time RMS and peak levels:

```bash
synapse -P 4447 listen mic levels
```

## Signal Probe

Capture a window and analyze per-channel signal properties (RMS, peak, noise floor, DC offset, channel correlation):

```bash
synapse -P 4447 listen mic probe --duration_ms 2000
```

## Channel Enumeration

List available channels with descriptive labels:

```bash
synapse -P 4447 listen mic list_channels
synapse -P 4447 listen mic list_channels --labels "host,guest"
```

## Per-Channel Streaming

Stream individual channels as mono audio with per-channel level metering:

```bash
# All channels
synapse -P 4447 listen mic stream_channels

# Single channel (TX1 only)
synapse -P 4447 listen mic stream_channels --channels 0

# Both channels with custom labels
synapse -P 4447 listen mic stream_channels --channels 0,1 --labels "host,guest"
```

Each tick emits `ChannelAudioData` (base64 mono PCM) and `ChannelLevel` (RMS/peak) per selected channel, with a shared `sequence` number for cross-channel correlation.

# yt-dlp captures

`sums/` holds release lists with their detached signatures, byte for byte
as published (`.gitattributes` keeps them out of any line-ending
conversion): yt-dlp's `SHA2-256SUMS` and `.sig` of a stable and a nightly
release, and `sums/node-v22.23.4/`, the `SHASUMS256.txt` and
`SHASUMS256.txt.sig` of `https://nodejs.org/dist/latest-v22.x/` downloaded on
2026-11-04 (v22.23.4, signed by the primary key
`5BE8A3F6C8A5C01D106C0AD820B1A390B168D356`; its armv7l tarball, not
committed, hashed to the listed
`4318e0c68f62e18583dae85eb645f55c7f2b8a2e8d1fffe91a2e8abd046880ab`).
`src/tool/verify.rs` and `src/tool/runtime.rs` read them in their tests.

Real answers of `yt-dlp --no-warnings -J --flat-playlist -- <url>`, captured on
2026-11-02 with yt-dlp 2026.10.26 (sha256 checked against the release's
SHA2-256SUMS), run on a workstation. `src/ytdlp.rs` reads them in its tests.

| File | Source | Changes |
|---|---|---|
| `video.json` | `https://www.youtube.com/watch?v=jNQXAC9IVRw` | The `formats`, `requested_formats`, `requested_downloads`, `heatmap`, `automatic_captions` and `subtitles` keys were removed (about 85 KB of the 92 KB). Nothing else. |
| `playlist.json` | `https://www.youtube.com/playlist?list=PLFgquLnL59alCl_2TQvOiD5Vgm1hCaGSI` (182 entries) | `entries` trimmed to the first 12. `playlist_count` still says 182, as yt-dlp wrote it. |
| `playlist-with-unavailable.json` | `https://www.youtube.com/playlist?list=PLYwq8WOe86_xGmR7FrcJq8Sb7VW8K3Tt2` (162 entries) | `entries` trimmed to 7 of the 162 (original positions 48, 49, 50, 54, 55, 60, 69; they therefore are not contiguous), chosen to hold two `[Private video]`, one `[Deleted video]`, and one available video whose real title starts with `[` (`[ORIGINAL] Kid cussing ...`). `playlist_count` still says 162. |

## Other sites

Captured on 2026-11-03 with the same yt-dlp 2026.10.26 (sha256
`e51e69e8...`, checked against the release's SHA2-256SUMS), as
`yt-dlp --ignore-config --no-warnings -J --flat-playlist -- <url>`, then
re-indented (one space, as the YouTube ones). The four URLs are yt-dlp's own
test URLs for these extractors.

| File | Source | Changes |
|---|---|---|
| `soundcloud-track.json` | `https://soundcloud.com/forss/flickermood` | Scrubbed (below). Nothing else. |
| `soundcloud-set.json` | `https://soundcloud.com/the-concept-band/sets/the-royal-concept-ep` (6 entries) | None. The flat entries carry no `title`, and the last one's `url` is an API address (`https://api-v2.soundcloud.com/tracks/47127631`): both as yt-dlp wrote them. |
| `bandcamp-track.json` | `https://youtube-dl.bandcamp.com/track/youtube-dl-test-song` | Scrubbed (below). Its real title holds `"`, `'`, `/`, `\`, `ä` and `↭`. |
| `bandcamp-album.json` | `https://blazo.bandcamp.com/album/jazz-format-mixtape-vol-1` (22 entries) | None: all 22 entries kept. |

**Scrubbed.** A single track's answer carries the signed media addresses
yt-dlp was handed for the workstation: Bandcamp's `t4.bcbits.com/stream/...`
with `ts=`, `t=` and `token=`, SoundCloud's
`playback.media-streaming.soundcloud.cloud/...` with CloudFront's `Policy`,
`Signature` and `Key-Pair-Id`, each repeated in `formats`, in
`requested_downloads` and, for the chosen format, at the top level (`url`,
with its `http_headers`). Those four keys were **removed** from both track
captures, rather than rewritten: nothing reads them, and a rewritten token
would still look like one. No other key held anything of the workstation:
the captures were searched, before the first commit, for its public address,
any dotted IPv4, `client_id`, `token=`, `Signature`, `Policy=`, `Key-Pair`,
`cookie`, `session` and the two media hosts, and none is left (the script
asserts it). The playlists' flat entries hold page addresses only, and their
`__x_forwarded_for_ip` is `null`.

`video-formats.json` is the answer mpv's `ytdl_hook` asks for under
`--no-video`, with its formats: `yt-dlp --ignore-config --no-warnings -J
--flat-playlist --format bestaudio/best -- https://www.youtube.com/watch?v=jNQXAC9IVRw`,
captured on 2026-11-03 with the same yt-dlp 2026.10.26 (sha256 `e51e69e8...`,
checked against the release's SHA2-256SUMS), one line as yt-dlp prints it.
Changes: 6 of the 24 `formats` kept (`233`, `234` HLS, whose `url` and
`manifest_url` are on `manifest.googlevideo.com`; `139`, `140`, `251` the
chosen one, `160`, on `rr4---sn-aaa-aaaaa.googlevideo.com` after the change below), `heatmap`,
`automatic_captions` and `subtitles` removed, and the workstation's public
address in the signed URLs (`ip=`, `/ip/`) replaced by `203.0.113.7`
(a documentation address), the session id (`ei=`, `/ei/`) by as many `A`,
and the names of the two edge nodes that served the workstation (`sn-*`,
in `mn=` and in the `rr4---sn-...googlevideo.com` host) by
`sn-aaa-aaaaa` and `sn-aaaaaaaa`: none of them says anything to the tests,
and together they located the workstation. `src/tool/wrapper.rs` reads it
in the tests of the network guard, with each of the two hosts replaced by a
public IP literal, so that no test resolves a name.

Not hand-edited otherwise: every value, including the unavailable entries,
is what yt-dlp emitted.

`local-network.txt` is not a capture: it is the table both network-guard
predicates (the wrapper's Python one, `src/netguard.rs`'s Rust one) are
tested against, line by line. Measured on those entries: an unavailable one has the title
`[Private video]` or `[Deleted video]` and `availability: null`, `duration:
null`, no channel; an available one of a flat playlist also has `availability:
null` (the badge is rarely present), so a null `availability` does not mean
"unavailable".

The video capture has thumbnails with no `width`/`height` (the `*.jpg` and
`*.webp` guessed from the id) and a few with sizes (signed `hqdefault.jpg`
URLs, up to 480x360); the flat playlist entries carry four sized ones.

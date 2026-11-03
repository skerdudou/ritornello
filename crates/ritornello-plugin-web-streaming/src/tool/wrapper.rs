//! The two files the plugin writes for yt-dlp: the wrapper `bin/yt-dlp`, and
//! the configuration `yt-dlp.conf` it hands yt-dlp.
//!
//! The wrapper is a Python script, not a shell one. It runs the extracted
//! package `<data_dir>/yt-dlp` in its own process by calling
//! `yt_dlp.main(...)`: one process, no grandchild, so the process-group kill
//! of the plugin's own calls and mpv's kill of its `ytdl_hook` both reach
//! everything. Its order:
//!
//! 1. `yt_dlp.main` with `--ignore-config` and, when the conf exists,
//!    `--config-locations <conf>` (yt-dlp still loads a file named that way
//!    under `--ignore-config`), followed by the caller's arguments, with no
//!    JavaScript runtime: measured on the device, a runtime only costs time
//!    today (none: 6.4-8.8 s per video, node 22: 26-29 s, QuickJS: ~125 s);
//! 2. only when that ended with a non-zero code, and a runtime was found,
//!    the same again with `--js-runtimes <name>:<path>`: kept for the day
//!    YouTube stops serving audio without one. The second call runs in the
//!    same process (the first has returned): measured on yt-dlp 2026.10.26,
//!    after a first call that failed on YouTube's answer, it gives the same
//!    answer as a fresh process.
//!
//! A failed `-J` prints `null` on stdout (measured). When a retry is
//! possible, the first attempt's stdout passes through as it comes, except
//! a leading `null` that is held back and dropped if the retry happens:
//! otherwise the retry's JSON would follow it, and neither the plugin nor
//! mpv could read the answer.
//!
//! It exits with the code of the last call made.
//!
//! A failed `-J` run served for **mpv** (the plugin's own runs carry
//! [`crate::ytdlp::OWN_RUN_ENV`]) is recorded in
//! [`crate::playfail::FILE`]: when, the host, a code, and yt-dlp's last line
//! on stderr, which the wrapper passes on to mpv as it comes while keeping
//! its tail. Nothing else would ever say why a playback stayed silent.
//!
//! # Never the local network
//!
//! The plugin opens links of any site yt-dlp reads, and the addresses it
//! then reaches come from remote data. Two guards live here, both needed
//! (the third, the plugin's own fetch of covers, is `netguard.rs`):
//!
//! - **Every connection of the process.** Before `yt_dlp` is imported,
//!   `socket.socket.connect`, `connect_ex`, `sendto` and `sendmsg` are
//!   replaced by versions that judge the **effective** address: a name is
//!   resolved first, and the numeric address that resolution gave is the one
//!   judged and the one connected to. Redirects, a DNS that changes its
//!   answer, a host spelled `127.1`: each ends in a connect, and each is
//!   judged there. A local one raises `PermissionError` (`EACCES`, "local
//!   network refused: <ip>"), which yt-dlp reports as a network error;
//!   `connect_ex` returns `EACCES`. `AF_UNIX` passes, any family other than
//!   IPv4 and IPv6 is refused. `is_local` is the predicate, held to the
//!   same table as the Rust one (`tests/fixtures/local-network.txt`).
//! - **What mpv will fetch itself.** mpv's `ytdl_hook` runs the wrapper with
//!   `-J` and then opens the stream addresses of the answer with its own
//!   network code, out of this process. When the arguments ask for `-J`
//!   (or `--dump-single-json`), the answer is kept whole instead of passed
//!   on, and judged before it is printed: the host of every address a
//!   format, the chosen format, a requested format or a subtitle would be
//!   fetched from is resolved (all at once, under [`RESOLVE_SECONDS`]), and
//!   whatever has one local address, or none in time, is dropped. A video
//!   left with nothing to play fails: nothing on stdout, "every format
//!   points to the local network" on stderr, a non-zero exit. A flat
//!   playlist's entries are page addresses and are left as they are: this
//!   filter does not judge them, and mpv may open an http one itself (see
//!   below) rather than through the hook.
//! - **No C extension past the guard.** The interpreter runs with `-IS`
//!   ([`PYTHON_FLAGS`]): no `site`, so no site-packages on the path, and
//!   `curl_cffi` is closed by name before `yt_dlp` is imported (libcurl
//!   would connect without Python's `socket`; yt-dlp skips that handler).
//!
//! **Residual risk, accepted.** Everything mpv fetches itself is out of this
//! process's reach. mpv (ffmpeg) resolves the stream's host again, follows
//! HTTP redirects, and reads m3u/pls playlists and HLS/DASH manifests whose
//! entries, segments and keys it then fetches: a hostile site, or a DNS
//! answer that changes between the two resolutions, can make mpv send **any
//! number of GETs, with any path and query, to hosts of the local network**
//! — enough to trigger a GET endpoint of a router or a NAS, the way a
//! cross-site request would. The answers are only ever played as audio:
//! nothing of them is sent anywhere. The link the plugin hands mpv is not
//! part of it: every address is written `ytdl://`, so mpv opens none itself
//! and goes through the hook (measured on mpv 0.37). What remains is the
//! http entries of a flat playlist the hook's answer expands: mpv opens them
//! itself, and the core keeps every `http(s)` address out of the hook's
//! fallback, so they should not come back through this wrapper either. That
//! last part is reasoned from mpv's source, not measured: during a web Play,
//! such entries have not been observed to fail or to skip the wrapper.
//!
//! The JavaScript runtime of the fallback (`--js-runtimes`) is the one
//! process yt-dlp starts: it runs outside the socket guard, handed YouTube's
//! player code, not addresses.

use std::path::Path;

/// The package directory the wrapper imports `yt_dlp` from.
pub const PACKAGE_DIR: &str = "yt-dlp";
pub const CONF: &str = "yt-dlp.conf";

/// The longest the resolution of a `-J` answer's hosts may take, in seconds
/// (all hosts at once). A host not resolved in time is treated as local.
pub const RESOLVE_SECONDS: f64 = 5.0;

fn utf8(path: &Path) -> Result<&str, String> {
    path.to_str().ok_or_else(|| format!("{} is not UTF-8", path.display()))
}

/// A Python string literal: a JSON string is one (same escapes, and the
/// characters JSON leaves raw are valid in a UTF-8 Python source).
fn py(s: &str) -> String {
    serde_json::to_string(s).expect("a string serialises")
}

/// The network guard of the wrapper, inserted before `import yt_dlp`: the
/// predicate, the replaced socket methods, and the judging of a `-J`
/// answer. Kept out of the `format!` of [`wrapper`] so its braces are
/// Python's.
const GUARD: &str = r#"
# ---- the network guard: never reach the local network ----------------------
import copy
import errno
import ipaddress
import json
import socket
import threading
import time
import urllib.parse

# No C extension past the guard. yt-dlp registers a curl_cffi request
# handler when it can import curl_cffi, and libcurl connects without
# Python's socket module. The shebang's -S already keeps the system's
# site-packages off the path; this closes the name wherever else it could
# come from (yt-dlp then skips that handler, as when it is not installed).
sys.modules["curl_cffi"] = None

_V4_LOCAL = tuple(ipaddress.IPv4Network(n) for n in (
    "0.0.0.0/8", "10.0.0.0/8", "100.64.0.0/10", "127.0.0.0/8", "169.254.0.0/16",
    "172.16.0.0/12", "192.168.0.0/16", "224.0.0.0/4", "240.0.0.0/4"))
_V6_LOCAL = tuple(ipaddress.IPv6Network(n) for n in (
    "::/96", "64:ff9b:1::/48", "fc00::/7", "fe80::/10", "fec0::/10", "ff00::/8"))
_MAPPED = ipaddress.IPv6Network("::ffff:0:0/96")
_NAT64 = ipaddress.IPv6Network("64:ff9b::/96")
_SIX_TO_FOUR = ipaddress.IPv6Network("2002::/16")


def is_local(ip):
    """An address of the local network, or one that would reach it. What
    cannot be read as an address is refused too. Written out rather than
    taken from ipaddress's is_private, whose definition is not this list
    (no 100.64/10) and has moved between Python versions."""
    if not isinstance(ip, str):
        return True
    try:
        a = ipaddress.ip_address(ip.split("%", 1)[0])
    except ValueError:
        return True
    if a.version == 4:
        return any(a in n for n in _V4_LOCAL)
    if a in _MAPPED:
        return is_local(str(ipaddress.IPv4Address(int(a) & 0xFFFFFFFF)))
    if a in _NAT64:
        return is_local(str(ipaddress.IPv4Address(int(a) & 0xFFFFFFFF)))
    if a in _SIX_TO_FOUR:
        return is_local(str(ipaddress.IPv4Address((int(a) >> 80) & 0xFFFFFFFF)))
    return any(a in n for n in _V6_LOCAL)


class LocalNetworkRefused(PermissionError):
    """An OSError: yt-dlp reports it as any network error."""


def _refused(what):
    return LocalNetworkRefused(errno.EACCES, "local network refused: %s" % (what,))


def _numeric(host):
    try:
        ipaddress.ip_address(host.split("%", 1)[0])
        return True
    except ValueError:
        return False


def _judged(sock, address):
    """The address `sock` may be connected or sent to. A name is resolved
    here, and the numeric address found is both the one judged and the one
    used: Python would otherwise resolve it again inside connect."""
    if sock.family == socket.AF_UNIX:
        return address
    if sock.family not in (socket.AF_INET, socket.AF_INET6):
        raise _refused("address family %s" % (sock.family,))
    if not isinstance(address, tuple) or len(address) < 2 or not isinstance(address[0], str):
        raise _refused(repr(address))
    if not _numeric(address[0]):
        found = socket.getaddrinfo(address[0], address[1], sock.family, sock.type)
        if not found:
            raise _refused(address[0])
        address = found[0][4]
    if is_local(address[0]):
        raise _refused(address[0])
    return address


_connect = socket.socket.connect
_connect_ex = socket.socket.connect_ex
_sendto = socket.socket.sendto
_sendmsg = socket.socket.sendmsg


def _guarded_connect(self, address):
    return _connect(self, _judged(self, address))


def _guarded_connect_ex(self, address):
    try:
        address = _judged(self, address)
    except LocalNetworkRefused:
        return errno.EACCES
    return _connect_ex(self, address)


def _guarded_sendto(self, data, *rest):
    if rest:
        rest = rest[:-1] + (_judged(self, rest[-1]),)
    return _sendto(self, data, *rest)


def _guarded_sendmsg(self, buffers, *rest):
    if len(rest) >= 3 and rest[2] is not None:
        rest = rest[:2] + (_judged(self, rest[2]),) + rest[3:]
    return _sendmsg(self, buffers, *rest)


socket.socket.connect = _guarded_connect
socket.socket.connect_ex = _guarded_connect_ex
socket.socket.sendto = _guarded_sendto
socket.socket.sendmsg = _guarded_sendmsg


ANSWER_FLAGS = ("-J", "--dump-single-json")
_FLAT = ("url", "url_transparent")
_SOURCES = ("url", "manifest_url", "fragment_base_url")


# yt-dlp's short options that take a value (measured on the parser of
# 2026.10.26): in a group such as -qJ, what follows one of them is its value,
# not more options; at the end of a group, the next argument is.
_SHORT_WITH_VALUE = "2INOPRSafoprtu"


def answers_json(args):
    """Whether this run prints a -J answer (mpv reads its streams from it):
    -J, --dump-single-json, or a group of short options holding J."""
    value_next = False
    for a in args:
        if value_next:
            value_next = False
            continue
        if a == "--":
            return False
        if a in ANSWER_FLAGS:
            return True
        if len(a) > 1 and a[0] == "-" and a[1] != "-":
            for i, c in enumerate(a[1:]):
                if c == "J":
                    return True
                if c in _SHORT_WITH_VALUE:
                    value_next = i == len(a) - 2
                    break
    return False


def _host(url):
    if not isinstance(url, str):
        return None
    try:
        return urllib.parse.urlsplit(url).hostname
    except ValueError:
        return None


def _sources(d):
    """The addresses one format (or a video's own top level, or a subtitle)
    is fetched from."""
    urls = [d[k] for k in _SOURCES if d.get(k) is not None]
    fragments = d.get("fragments")
    base = d.get("fragment_base_url")
    if isinstance(fragments, list):
        for f in fragments:
            if not isinstance(f, dict):
                continue
            if f.get("url") is not None:
                urls.append(f["url"])
            path = f.get("path")
            if path is not None:
                # Joined as mpv's ytdl_hook joins them: plain concatenation,
                # so "@192.168.0.1/x" after "https://8.8.8.8" names another
                # host. A path without a base cannot be judged: refused.
                both = isinstance(base, str) and isinstance(path, str)
                urls.append(base + path if both else None)
    return urls


def _guard_video(v, ok):
    """Drops from one video what reaches the local network. False when it had
    something to play and nothing is left."""
    playable = "url" in v or bool(v.get("requested_formats"))
    formats = v.get("formats")
    had_formats = isinstance(formats, list) and len(formats) > 0
    if isinstance(formats, list):
        v["formats"] = [f for f in formats if isinstance(f, dict) and ok(_sources(f))]
    requested = v.get("requested_formats")
    if isinstance(requested, list) and not all(isinstance(f, dict) and ok(_sources(f)) for f in requested):
        del v["requested_formats"]
    if not ok(_sources(v)):
        for k in _SOURCES + ("fragments",):
            v.pop(k, None)
    subtitles = v.get("requested_subtitles")
    if isinstance(subtitles, dict):
        v["requested_subtitles"] = {k: s for k, s in subtitles.items() if isinstance(s, dict) and ok(_sources(s))}
    if had_formats and not v["formats"]:
        return False
    return not playable or "url" in v or bool(v.get("requested_formats"))


def _guard_doc(doc, ok):
    """The same for one answer: a video, or a playlist whose entries that are
    not flat are videos (an entry left with nothing is dropped). A flat
    entry is a page address and is left: mpv loads it later, through this
    wrapper or (an http one) by itself; see the residual risk."""
    if not isinstance(doc, dict) or doc.get("_type") in _FLAT:
        return True
    if doc.get("_type") in ("playlist", "multi_video"):
        entries = doc.get("entries")
        if not isinstance(entries, list):
            return True
        kept = [e for e in entries if _guard_doc(e, ok)]
        doc["entries"] = kept
        return len(kept) > 0 or len(entries) == 0
    return _guard_video(doc, ok)


def _resolve_all(hosts):
    """Each host's addresses, all resolved at once under one deadline: None
    for a host that failed or did not answer in time."""
    found = {}

    def one(h):
        try:
            found[h] = [i[4][0] for i in socket.getaddrinfo(h, None, 0, socket.SOCK_STREAM)]
        except Exception:
            found[h] = None

    threads = [threading.Thread(target=one, args=(h,), daemon=True) for h in hosts]
    for t in threads:
        t.start()
    deadline = time.monotonic() + RESOLVE_SECONDS
    for t in threads:
        t.join(max(0.0, deadline - time.monotonic()))
    return {h: found.get(h) for h in hosts}


def guard_answer(data):
    """A -J answer with what reaches the local network taken out: (bytes,
    None), the original bytes when nothing changed; or (None, why) when a
    video is left with nothing to play or the answer cannot be read."""
    try:
        text = data.decode("utf-8")
        decoder, docs, i = json.JSONDecoder(), [], 0
        while True:
            while i < len(text) and text[i].isspace():
                i += 1
            if i == len(text):
                break
            doc, i = decoder.raw_decode(text, i)
            docs.append(doc)
    except ValueError:
        return None, "the answer to judge is not JSON"
    # A live stream never ends: refused at play time as at the probe
    # (`ytdlp::LIVE_REFUSAL` reads this very sentence back).
    for d in docs:
        if isinstance(d, dict) and (d.get("is_live") is True or d.get("live_status") in ("is_live", "is_upcoming")):
            return None, "a live stream is not played"
    hosts = set()

    def collect(urls):
        hosts.update(h for h in map(_host, urls) if h)
        return True

    for d in docs:
        _guard_doc(copy.deepcopy(d), collect)
    addresses = _resolve_all(sorted(hosts))

    def ok(urls):
        for u in urls:
            found = addresses.get(_host(u))
            if not found or any(is_local(a) for a in found):
                return False
        return True

    out, changed = [], False
    for d in docs:
        before = json.dumps(d, sort_keys=True)
        if not _guard_doc(d, ok):
            return None, "every format points to the local network"
        changed = changed or json.dumps(d, sort_keys=True) != before
        out.append(json.dumps(d))
    if not changed:
        return data, None
    return ("\n".join(out) + "\n").encode(), None
# ---- end of the network guard ----------------------------------------------
"#;

/// The record of a failed playback (`crate::playfail`), inserted after the
/// guard (it uses its `json`, `time` and `_host`). Kept out of the `format!`
/// of [`wrapper`] so its braces are Python's.
const RECORD: &str = r#"
# ---- the last playback failure, for the page and the source ----------------
class Tail(io.RawIOBase):
    """Passes stderr on as it comes (mpv logs it), keeping its last bytes:
    what yt-dlp said when it failed."""

    KEEP = 4096

    def __init__(self, out):
        self.out = out
        self.tail = b""

    def writable(self):
        return True

    def isatty(self):
        return self.out.isatty()

    def fileno(self):
        return self.out.fileno()

    def write(self, b):
        data = bytes(b)
        self.out.write(data)
        self.out.flush()
        self.tail = (self.tail + data)[-self.KEEP:]
        return len(data)


def failure_code(why, said):
    """The code of a failed run: the guard's own refusals by their sentence,
    the socket guard's by its message in yt-dlp's words, then what yt-dlp
    says when a site offers nothing to play."""
    if why == "a live stream is not played":
        return "live_refused"
    if why == "every format points to the local network" or "local network refused" in said:
        return "guard_refused"
    if "Requested format is not available" in said or "No video formats found" in said:
        return "no_formats"
    return "ytdlp_failed"


def address_of(args):
    """The address a run was asked about: what follows "--", or the last
    argument."""
    if "--" in args:
        rest = args[args.index("--") + 1:]
        return rest[0] if rest else ""
    return args[-1] if args else ""


def record_failure(args, why, said):
    lines = [l.strip() for l in said.splitlines() if l.strip()]
    record = {
        "time": time.time(),
        "host": _host(address_of(args)) or "",
        "code": failure_code(why, said),
        "detail": lines[-1][:500] if lines else "",
    }
    tmp = "%s.tmp.%d" % (FAILURE, os.getpid())
    try:
        with open(tmp, "w", encoding="utf-8") as f:
            json.dump(record, f)
        os.replace(tmp, FAILURE)
    except OSError:
        try:
            os.unlink(tmp)
        except OSError:
            pass
# ---- end of the record ------------------------------------------------------
"#;

/// How the wrapper's interpreter runs, as one argument (a shebang carries
/// only one): `-I`, isolated from `PYTHONPATH`, the user's site packages and
/// the current directory; `-S`, no `site` at all, so the system's
/// site-packages are not on the path either. Nothing there is needed: the
/// zipapp is yt-dlp whole (measured: a `-J` and a download of 2026.10.26
/// under `-IS`), and what could be found there are C extensions that open
/// connections past the guard (`curl_cffi`) or code nobody here chose.
pub const PYTHON_FLAGS: &str = "-IS";

/// The wrapper. `python3` is the interpreter the plugin probed (absolute):
/// written into the shebang with [`PYTHON_FLAGS`], so whoever runs the
/// wrapper (mpv, with its own `PATH`) gets the same interpreter the warm-up
/// compiled the bytecode with, isolated from the environment and from any
/// site-packages.
/// `runtime` is `<name>:<absolute path>`, as `--js-runtimes` takes it, or
/// `None` when the machine has no usable runtime.
pub fn wrapper(data_dir: &Path, python3: &Path, runtime: Option<&str>) -> Result<String, String> {
    let package = utf8(&data_dir.join(PACKAGE_DIR))?.to_string();
    let failure = utf8(&data_dir.join(crate::playfail::FILE))?.to_string();
    let conf = utf8(&data_dir.join(CONF))?.to_string();
    check_interpreter(python3)?;
    let interpreter = utf8(python3)?;
    let runtime = runtime.map_or_else(|| "None".to_string(), py);
    Ok(format!(
        r#"#!{interpreter} {PYTHON_FLAGS}
# Written by ritornello-plugin-web-streaming, rewritten at each start and install: do not edit.
import io
import os
import sys

PACKAGE = {package}
CONF = {conf}
RUNTIME = {runtime}
RESOLVE_SECONDS = {RESOLVE_SECONDS:?}
FAILURE = {failure}
OWN_RUN = {own_run}
{GUARD}{RECORD}
# Inserted, not put in place of sys.path[0]: under -I (Python 3.11 and
# later) the script's directory is not on the path, and sys.path[0] is the
# standard library's.
sys.path.insert(0, PACKAGE)
import yt_dlp


class HoldNull(io.RawIOBase):
    """Passes stdout through as it comes, except what could still be the
    "null" a failed -J prints: a retry must not leave it in front of the
    JSON of the second attempt."""

    NULL = b"null\n"

    def __init__(self, out):
        self.out = out
        self.held = b""
        self.passing = False

    def writable(self):
        return True

    def isatty(self):
        return self.out.isatty()

    def fileno(self):
        return self.out.fileno()

    def write(self, b):
        n = len(b)
        data = bytes(b)
        if not self.passing:
            data = self.held + data
            if self.NULL.startswith(data):
                self.held = data
                return n
            self.passing, self.held = True, b""
        self.out.write(data)
        self.out.flush()
        return n


class Capture(io.RawIOBase):
    """Keeps a -J answer whole: it is judged before any reader sees it."""

    def __init__(self, out):
        self.out = out
        self.data = bytearray()

    def writable(self):
        return True

    def isatty(self):
        return False

    def fileno(self):
        return self.out.fileno()

    def write(self, b):
        self.data += bytes(b)
        return len(b)


def run(args, mode):
    """One call of yt_dlp.main, its stdout passed on as it comes ("pass"),
    with a leading null held back ("hold"), or kept whole ("capture")."""
    real = sys.stdout
    raw = None
    if mode != "pass":
        raw = HoldNull(real.buffer) if mode == "hold" else Capture(real.buffer)
        sys.stdout = io.TextIOWrapper(
            io.BufferedWriter(raw), encoding=real.encoding, errors=real.errors, line_buffering=True)
    try:
        yt_dlp.main(args)
        code = 0
    except SystemExit as e:
        code = e.code
    finally:
        if raw is not None:
            sys.stdout.flush()
            sys.stdout = real
    return code, raw


def failed(code):
    return code is not None and code != 0


base = ["--ignore-config"]
if os.path.isfile(CONF):
    base += ["--config-locations", CONF]
args = sys.argv[1:]
capture = answers_json(args)
# mpv's runs: their failure is recorded, and their stderr's tail kept for it.
tail = None
if capture and OWN_RUN not in os.environ:
    real_err = sys.stderr
    tail = Tail(real_err.buffer)
    sys.stderr = io.TextIOWrapper(
        io.BufferedWriter(tail), encoding=real_err.encoding, errors=real_err.errors, line_buffering=True)
mode = "capture" if capture else ("hold" if RUNTIME is not None else "pass")
code, first = run(base + args, mode)
if failed(code) and RUNTIME is not None:
    code, first = run(base + ["--js-runtimes", RUNTIME] + args, "capture" if capture else "pass")
elif mode == "hold" and first.held:
    sys.stdout.buffer.write(first.held)
why = None
if capture:
    answer, why = guard_answer(bytes(first.data))
    if answer is None:
        sys.stderr.write("ERROR: " + why + "\n")
        if not failed(code):
            code = 1
    else:
        sys.stdout.buffer.write(answer)
sys.stdout.flush()
if tail is not None and failed(code):
    sys.stderr.flush()
    record_failure(args, why, tail.tail.decode("utf-8", "replace"))
sys.exit(code)
"#,
        package = py(&package),
        conf = py(&conf),
        failure = py(&failure),
        own_run = py(crate::ytdlp::OWN_RUN_ENV),
    ))
}

/// The longest shebang line older kernels read whole (`BINPRM_BUF_SIZE` was
/// 128 bytes, newline included, before Linux 5.1): a longer one is cut,
/// and the interpreter named would be another.
pub const SHEBANG_MAX: usize = 127;

/// `python3` can be the interpreter of the wrapper's shebang: an absolute
/// UTF-8 path, without whitespace (a shebang has one argument), and a line
/// `#!<path> <PYTHON_FLAGS>` that fits in [`SHEBANG_MAX`].
pub fn check_interpreter(python3: &Path) -> Result<(), String> {
    let interpreter = utf8(python3)?;
    if !python3.is_absolute() || interpreter.chars().any(char::is_whitespace) {
        return Err(format!("{interpreter:?} cannot be named in a shebang"));
    }
    let line = format!("#!{interpreter} {PYTHON_FLAGS}");
    if line.len() > SHEBANG_MAX {
        return Err(format!("{interpreter:?} makes a shebang of {} bytes, more than {SHEBANG_MAX}", line.len()));
    }
    Ok(())
}

/// Quotes a value for yt-dlp's configuration files (read with `shlex`).
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r#"'"'"'"#))
}

/// The configuration: yt-dlp's cache (the deciphering it learns) under the
/// plugin's data directory, the only place the service may write.
pub fn conf(data_dir: &Path) -> Result<String, String> {
    let cache = utf8(&data_dir.join("cache"))?.to_string();
    Ok(format!("# Written by ritornello-plugin-web-streaming: do not edit.\n--cache-dir {}\n", shell_quote(&cache)))
}

/// What warms a freshly extracted package up before it is put in place:
/// `python3 <these>` imports it from `package_dir` (and so compiles its
/// bytecode there, which outlives the directory's rename) and prints the
/// version. [`PYTHON_FLAGS`]: what the wrapper itself runs with.
pub fn warm_up_args(package_dir: &Path) -> Result<Vec<String>, String> {
    Ok(vec![
        PYTHON_FLAGS.into(),
        "-c".into(),
        "import sys; sys.path.insert(0, sys.argv[1]); import yt_dlp; yt_dlp.main(['--ignore-config', '--version'])"
            .into(),
        utf8(package_dir)?.to_string(),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::testkit::{real_python3, retrying};
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    /// A fake `yt_dlp` whose `main` records its arguments (one JSON line per
    /// call), prints `FAKE_OUT_<n>` on its n-th call the way yt-dlp writes
    /// (to `sys.stdout.buffer`, then a flush), waits for the file
    /// `FAKE_WAIT_<n>` when set, and exits with the codes of `FAKE_CODES`.
    /// `FAKE_OUTFILE_<n>` names a file printed instead of `FAKE_OUT_<n>`.
    /// `FAKE_ERR_<n>` is written on stderr the way yt-dlp's `write_string`
    /// writes (to `sys.stderr.buffer`, then a flush).
    const RECORDING_MAIN: &str = r#"import json, os, sys, time

LOG = os.environ["FAKE_LOG"]


def main(argv=None):
    pathlog = os.environ.get("FAKE_PATHLOG")
    if pathlog:
        with open(pathlog, "w") as f:
            f.write(json.dumps(sys.path))
    with open(LOG, "a") as f:
        f.write(json.dumps(argv) + "\n")
    with open(LOG) as f:
        n = sum(1 for _ in f)
    out = os.environ.get(f"FAKE_OUT_{n}")
    outfile = os.environ.get(f"FAKE_OUTFILE_{n}")
    if outfile:
        with open(outfile, "rb") as f:
            out = f.read().decode()
    if out:
        sys.stdout.buffer.write(out.encode())
        sys.stdout.flush()
    err = os.environ.get(f"FAKE_ERR_{n}")
    if err:
        sys.stderr.buffer.write(err.encode())
        sys.stderr.flush()
    wait = os.environ.get(f"FAKE_WAIT_{n}")
    if wait:
        for _ in range(200):
            if os.path.exists(wait):
                break
            time.sleep(0.05)
    codes = os.environ.get("FAKE_CODES", "0").split(",")
    code = codes[min(n, len(codes)) - 1]
    if code == "return":
        return None
    raise SystemExit(int(code))
"#;

    struct Rig {
        dir: tempfile::TempDir,
        bin: std::path::PathBuf,
        log: std::path::PathBuf,
    }

    fn rig(runtime: Option<&str>, with_conf: bool) -> Rig {
        rig_with(runtime, with_conf, RECORDING_MAIN)
    }

    /// The same, with `main` as the fake `yt_dlp` package.
    fn rig_with(runtime: Option<&str>, with_conf: bool, main: &str) -> Rig {
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path().join(PACKAGE_DIR).join("yt_dlp");
        std::fs::create_dir_all(&pkg).unwrap();
        std::fs::write(pkg.join("__init__.py"), main).unwrap();
        std::fs::create_dir(dir.path().join("bin")).unwrap();
        let bin = dir.path().join("bin/yt-dlp");
        std::fs::write(&bin, wrapper(dir.path(), &real_python3(), runtime).unwrap()).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        if with_conf {
            std::fs::write(dir.path().join(CONF), conf(dir.path()).unwrap()).unwrap();
        }
        let log = dir.path().join("calls.jsonl");
        Rig { dir, bin, log }
    }

    /// Runs the wrapper through its shebang, as mpv and the plugin do.
    fn run(r: &Rig, codes: &str, args: &[&str]) -> (i32, Vec<Vec<String>>) {
        let (code, calls, _) = run_with(r, codes, args, &[]);
        (code, calls)
    }

    fn command(r: &Rig, codes: &str, args: &[&str], env: &[(&str, &str)]) -> Command {
        real_python3(); // the shebang finds the same python3 on PATH
        let mut c = Command::new(&r.bin);
        c.args(args).env("FAKE_LOG", &r.log).env("FAKE_CODES", codes);
        for (k, v) in env {
            c.env(k, v);
        }
        c
    }

    fn run_with(r: &Rig, codes: &str, args: &[&str], env: &[(&str, &str)]) -> (i32, Vec<Vec<String>>, String) {
        let mut c = command(r, codes, args, env);
        let out = retrying(|| c.output());
        let calls = std::fs::read_to_string(&r.log)
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        (out.status.code().unwrap(), calls, String::from_utf8(out.stdout).unwrap())
    }

    /// Measured on yt-dlp 2026.10.26: a failed `-J` prints `null` on stdout.
    /// Left there, the retry's JSON would follow it and no reader (the
    /// plugin's probe, mpv's ytdl_hook) could parse the answer.
    #[test]
    fn a_retry_leaves_no_null_in_front_of_its_answer() {
        let r = rig(Some("node:/usr/bin/node"), false);
        let (code, calls, out) =
            run_with(&r, "1,0", &["-J"], &[("FAKE_OUT_1", "null\n"), ("FAKE_OUT_2", "{\"id\": \"x\"}\n")]);
        assert_eq!((code, calls.len()), (0, 2));
        assert_eq!(out, "{\"id\": \"x\"}\n");
        // Both attempts failing: one null, the second's.
        let r = rig(Some("node:/usr/bin/node"), false);
        let (code, _, out) = run_with(&r, "1,1", &["-J"], &[("FAKE_OUT_1", "null\n"), ("FAKE_OUT_2", "null\n")]);
        assert_eq!((code, out.as_str()), (1, "null\n"));
        // Without -J (the answer is passed on as it comes, not kept whole):
        // the held null is dropped all the same.
        let r = rig(Some("node:/usr/bin/node"), false);
        let (code, calls, out) =
            run_with(&r, "1,0", &["--print", "x"], &[("FAKE_OUT_1", "null\n"), ("FAKE_OUT_2", "{\"id\": \"x\"}\n")]);
        assert_eq!((code, calls.len(), out.as_str()), (0, 2, "{\"id\": \"x\"}\n"));
    }

    #[test]
    fn what_is_not_retried_is_printed_whole() {
        // A success that printed null keeps it.
        let r = rig(Some("node:/usr/bin/node"), false);
        let (code, calls, out) = run_with(&r, "0", &["-J"], &[("FAKE_OUT_1", "null\n")]);
        assert_eq!((code, calls.len(), out.as_str()), (0, 1, "null\n"));
        // A failure with nothing to retry with keeps it.
        let r = rig(None, false);
        let (code, _, out) = run_with(&r, "1", &["-J"], &[("FAKE_OUT_1", "null\n")]);
        assert_eq!((code, out.as_str()), (1, "null\n"));
        // Output that is not null passes, held or not (without -J: a -J
        // answer is read as JSON before it is printed).
        let r = rig(Some("node:/usr/bin/node"), false);
        let (_, _, out) = run_with(&r, "0", &["--print", "x"], &[("FAKE_OUT_1", "nullx\n{}\n")]);
        assert_eq!(out, "nullx\n{}\n");
        let r = rig(Some("node:/usr/bin/node"), false);
        let (_, _, out) = run_with(&r, "0", &["--print", "x"], &[("FAKE_OUT_1", "nul")]);
        assert_eq!(out, "nul");
    }

    /// The first attempt's progress reaches the reader while it runs: a
    /// download's percentages are read line by line.
    #[test]
    fn the_first_attempts_output_is_passed_on_as_it_comes() {
        use std::io::BufRead;
        let r = rig(Some("node:/usr/bin/node"), false);
        let go = r.dir.path().join("go");
        let go_s = go.to_str().unwrap().to_string();
        let mut c = command(&r, "0", &["-o", "x"], &[("FAKE_OUT_1", "[download]   1.0%\n"), ("FAKE_WAIT_1", &go_s)]);
        c.stdout(std::process::Stdio::piped());
        let mut child = retrying(|| c.spawn());
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            let _ = std::io::BufReader::new(stdout).read_line(&mut line);
            let _ = tx.send(line);
        });
        let line = rx.recv_timeout(std::time::Duration::from_secs(5));
        std::fs::write(&go, "").unwrap();
        child.wait().unwrap();
        assert_eq!(line.as_deref(), Ok("[download]   1.0%\n"), "held until the attempt ended");
    }

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_failure_is_retried_once_with_the_runtime_and_the_second_code_is_the_exit() {
        let r = rig(Some("node:/usr/bin/node"), false);
        let (code, calls) = run(&r, "1,3", &["-J", "--", "URL"]);
        assert_eq!(code, 3, "the second call's code");
        assert_eq!(
            calls,
            vec![
                strs(&["--ignore-config", "-J", "--", "URL"]),
                strs(&["--ignore-config", "--js-runtimes", "node:/usr/bin/node", "-J", "--", "URL"]),
            ]
        );
        let r = rig(Some("node:/usr/bin/node"), false);
        assert_eq!(run(&r, "1,0", &["-J"]).0, 0);
    }

    #[test]
    fn a_success_is_not_retried() {
        let r = rig(Some("deno:/data/bin/deno"), false);
        let (code, calls) = run(&r, "0,5", &["--version"]);
        assert_eq!(code, 0);
        assert_eq!(calls, vec![strs(&["--ignore-config", "--version"])]);
        // A `main` that returns instead of exiting succeeded too.
        let r = rig(Some("deno:/data/bin/deno"), false);
        let (code, calls) = run(&r, "return,5", &["--version"]);
        assert_eq!((code, calls.len()), (0, 1));
    }

    #[test]
    fn without_a_runtime_a_failure_is_final() {
        let r = rig(None, false);
        let (code, calls) = run(&r, "1,0", &["-J"]);
        assert_eq!(code, 1);
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn the_conf_is_named_only_when_it_exists() {
        let r = rig(None, true);
        let conf_path = r.dir.path().join(CONF).to_str().unwrap().to_string();
        let (_, calls) = run(&r, "0", &["-J"]);
        assert_eq!(calls, vec![vec!["--ignore-config".to_string(), "--config-locations".into(), conf_path, "-J".into()]]);
    }

    /// The probed interpreter, isolated: a `PYTHONPATH` would come before the
    /// standard library, so a planted `json.py` would replace the one the
    /// package imports. Under `-I` it is not read.
    #[test]
    fn the_wrapper_runs_the_probed_python3_isolated() {
        let r = rig(None, false);
        let text = std::fs::read_to_string(&r.bin).unwrap();
        assert_eq!(text.lines().next().unwrap(), format!("#!{} {PYTHON_FLAGS}", real_python3().display()));
        let pythonpath = r.dir.path().join("planted");
        std::fs::create_dir_all(&pythonpath).unwrap();
        std::fs::write(pythonpath.join("json.py"), "raise SystemExit(42)\n").unwrap();
        let (code, calls, _) = run_with(&r, "0", &["-J"], &[("PYTHONPATH", pythonpath.to_str().unwrap())]);
        assert_eq!((code, calls.len()), (0, 1), "the planted package ran");
        // An interpreter that cannot be named in a shebang is refused.
        assert!(wrapper(Path::new("/d"), Path::new("python3"), None).is_err());
        assert!(wrapper(Path::new("/d"), Path::new("/a b/python3"), None).is_err());
        // `#!` + path + ` -IS` fits in 127 bytes, not one more.
        let at_limit = format!("/{}", "a".repeat(SHEBANG_MAX - 7));
        assert_eq!(format!("#!{at_limit} {PYTHON_FLAGS}").len(), SHEBANG_MAX);
        assert_eq!(check_interpreter(Path::new(&at_limit)), Ok(()));
        let over = format!("{at_limit}a");
        assert!(check_interpreter(Path::new(&over)).is_err());
        assert!(wrapper(Path::new("/d"), Path::new(&over), None).is_err());
    }

    /// The package is inserted in front, and the interpreter's own path under the same flags
    /// (the standard library's entries, the first of them included) follows
    /// unchanged: replacing `sys.path[0]` would drop that first entry.
    #[test]
    fn the_package_comes_first_and_the_interpreters_own_path_follows_unchanged() {
        let r = rig(None, false);
        let log = r.dir.path().join("path.json");
        let (code, _, _) = run_with(&r, "0", &["-J"], &[("FAKE_PATHLOG", log.to_str().unwrap())]);
        assert_eq!(code, 0);
        let got: Vec<String> = serde_json::from_str(&std::fs::read_to_string(&log).unwrap()).unwrap();
        let out = Command::new(real_python3())
            .args([PYTHON_FLAGS, "-c", "import sys, json; print(json.dumps(sys.path))"])
            .output()
            .unwrap();
        let own: Vec<String> = serde_json::from_slice(&out.stdout).unwrap();
        let mut expected = vec![r.dir.path().join(PACKAGE_DIR).to_str().unwrap().to_string()];
        expected.extend(own);
        assert_eq!(got, expected);
    }

    #[test]
    fn odd_characters_in_the_paths_stay_inside_their_literals() {
        let text = wrapper(Path::new("/data/it's \"x\"\\y"), Path::new("/usr/bin/python3"), Some("node:/a b/node")).unwrap();
        assert!(text.contains(r#"PACKAGE = "/data/it's \"x\"\\y/yt-dlp""#), "{text}");
        assert!(text.contains(r#"RUNTIME = "node:/a b/node""#), "{text}");
        assert_eq!(conf(Path::new("/d/it's")).unwrap().lines().nth(1), Some(r#"--cache-dir '/d/it'"'"'s/cache'"#));
    }

    // ---- the network guard ---------------------------------------------------

    /// A fake `yt_dlp` whose `main` asks the wrapper's own predicate
    /// (`__main__.is_local`: the wrapper runs as `__main__`) about each
    /// address of `FAKE_TABLE` (a JSON list), then about two values that are
    /// not strings, and writes the answers to `FAKE_RESULT`.
    const PREDICATE_MAIN: &str = r#"import json, os
import __main__


def main(argv=None):
    with open(os.environ["FAKE_TABLE"]) as f:
        table = json.load(f)
    answers = [__main__.is_local(ip) for ip in table]
    answers += [__main__.is_local(None), __main__.is_local(2130706433)]
    with open(os.environ["FAKE_RESULT"], "w") as f:
        json.dump(answers, f)
    raise SystemExit(0)
"#;

    /// The table the Rust predicate is held to (`netguard.rs`), line by
    /// line: the two cannot drift apart.
    #[test]
    fn the_wrappers_predicate_agrees_with_the_shared_table_line_by_line() {
        let table = crate::netguard::tests::table();
        let r = rig_with(None, false, PREDICATE_MAIN);
        let (input, result) = (r.dir.path().join("table.json"), r.dir.path().join("result.json"));
        let ips: Vec<&str> = table.iter().map(|(ip, _)| ip.as_str()).collect();
        std::fs::write(&input, serde_json::to_string(&ips).unwrap()).unwrap();
        let env = [("FAKE_TABLE", input.to_str().unwrap()), ("FAKE_RESULT", result.to_str().unwrap())];
        let (code, _, _) = run_with(&r, "0", &[], &env);
        assert_eq!(code, 0);
        let answers: Vec<bool> = serde_json::from_str(&std::fs::read_to_string(&result).unwrap()).unwrap();
        assert_eq!(answers.len(), table.len() + 2);
        let wrong: Vec<String> = table
            .iter()
            .zip(&answers)
            .filter(|((_, local), got)| local != *got)
            .map(|((ip, local), _)| format!("{ip}: expected {}", if *local { "local" } else { "public" }))
            .collect();
        assert!(wrong.is_empty(), "{wrong:#?}");
        assert_eq!(answers[table.len()..], [true, true], "what is not a string is refused");
    }

    /// A fake `yt_dlp` whose `main` tries every way the process could reach
    /// a listener of the test on 127.0.0.1, records how each ended in
    /// `FAKE_RESULT`, and ends as yt-dlp would meet a refusal it did not
    /// catch.
    const CONNECTING_MAIN: &str = r#"import errno, json, os, socket
import __main__


def main(argv=None):
    port = int(os.environ["FAKE_PORT"])
    udp = int(os.environ["FAKE_UDP"])
    seen = {}

    def attempt(name, f):
        try:
            seen[name] = ["ok", f()]
        except OSError as e:
            seen[name] = ["refused" if e.errno == errno.EACCES else "error", str(e)]

    def tcp(host, family=socket.AF_INET):
        s = socket.socket(family, socket.SOCK_STREAM)
        s.settimeout(2)
        try:
            s.connect((host, port))
        finally:
            s.close()

    def tcp_ex(host):
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.settimeout(2)
        try:
            return s.connect_ex((host, port))
        finally:
            s.close()

    def datagram(send):
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        try:
            return send(s)
        finally:
            s.close()

    def unix():
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            s.connect(os.environ["FAKE_UNIX"])
        finally:
            s.close()

    attempt("connect", lambda: tcp("127.0.0.1"))
    attempt("connect_by_name", lambda: tcp("localhost"))
    attempt("connect_short_spelling", lambda: tcp("127.1"))
    attempt("connect_mapped", lambda: tcp("::ffff:127.0.0.1", socket.AF_INET6))
    attempt("create_connection", lambda: socket.create_connection(("127.0.0.1", port), timeout=2).close())
    attempt("connect_ex", lambda: tcp_ex("127.0.0.1"))
    attempt("sendto", lambda: datagram(lambda s: s.sendto(b"x", ("127.0.0.1", udp))))
    attempt("sendto_flags", lambda: datagram(lambda s: s.sendto(b"x", 0, ("127.0.0.1", udp))))
    attempt("sendmsg", lambda: datagram(lambda s: s.sendmsg([b"x"], [], 0, ("127.0.0.1", udp))))
    attempt("unix", unix)

    def netlink():
        s = socket.socket(socket.AF_NETLINK, socket.SOCK_RAW, 0)
        try:
            s.connect((0, 0))
        finally:
            s.close()

    attempt("other_family", netlink)
    # The allow path, without the network: a name is resolved before it is
    # judged, and the address found is the one connected to ("134744072"
    # is 8.8.8.8 to the resolver, not to ipaddress).
    judged = getattr(__main__, "_judged", None)
    if judged:
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        attempt("judged_by_name", lambda: list(judged(s, ("134744072", 80))))
        attempt("judged_literal", lambda: list(judged(s, ("8.8.8.8", 80))))
        s.close()
    with open(os.environ["FAKE_RESULT"], "w") as f:
        json.dump(seen, f)
    socket.create_connection(("127.0.0.1", port), timeout=2).close()
"#;

    #[test]
    fn no_connection_of_the_process_reaches_the_local_network() {
        use std::io::ErrorKind::WouldBlock;
        use std::net::{TcpListener, UdpSocket};
        use std::os::unix::net::UnixListener;
        let r = rig_with(None, false, CONNECTING_MAIN);
        let tcp = TcpListener::bind("127.0.0.1:0").unwrap();
        tcp.set_nonblocking(true).unwrap();
        let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
        udp.set_nonblocking(true).unwrap();
        let unix_path = r.dir.path().join("u.sock");
        let unix = UnixListener::bind(&unix_path).unwrap();
        unix.set_nonblocking(true).unwrap();
        let (port, udp_port) = (tcp.local_addr().unwrap().port().to_string(), udp.local_addr().unwrap().port().to_string());
        let (result, control) = (r.dir.path().join("result.json"), r.dir.path().join("control.json"));
        let env = |result: &Path| {
            vec![
                ("FAKE_PORT", port.clone()),
                ("FAKE_UDP", udp_port.clone()),
                ("FAKE_UNIX", unix_path.to_str().unwrap().to_string()),
                ("FAKE_RESULT", result.to_str().unwrap().to_string()),
            ]
        };

        let guarded = env(&result);
        let guarded: Vec<(&str, &str)> = guarded.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let mut c = command(&r, "0", &[], &guarded);
        let out = retrying(|| c.output());
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_ne!(out.status.code(), Some(0), "{stderr}");
        assert!(stderr.contains("local network refused: 127.0.0.1"), "{stderr}");
        let seen: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&result).unwrap()).unwrap();
        for name in [
            "connect",
            "connect_by_name",
            "connect_short_spelling",
            "connect_mapped",
            "create_connection",
            "sendto",
            "sendto_flags",
            "sendmsg",
            "other_family",
        ] {
            assert_eq!(seen[name][0], "refused", "{name}: {}", seen[name]);
            assert!(seen[name][1].as_str().unwrap().contains("local network refused"), "{name}: {}", seen[name]);
        }
        assert_eq!(seen["judged_by_name"], serde_json::json!(["ok", ["8.8.8.8", 80]]));
        assert_eq!(seen["judged_literal"], serde_json::json!(["ok", ["8.8.8.8", 80]]));
        assert_eq!(seen["connect_ex"], serde_json::json!(["ok", libc::EACCES]));
        assert_eq!(seen["unix"], serde_json::json!(["ok", null]), "a Unix socket passes");
        assert_eq!(tcp.accept().map(|_| ()).unwrap_err().kind(), WouldBlock, "a connection reached the listener");
        assert_eq!(udp.recv(&mut [0u8; 8]).unwrap_err().kind(), WouldBlock, "a datagram reached the listener");
        assert!(unix.accept().is_ok());

        // The control: the same package, run without the wrapper, reaches
        // them all (so the refusals above are the guard's, not the test's).
        let pkg = r.dir.path().join(PACKAGE_DIR);
        let mut c = Command::new(real_python3());
        c.args(["-I", "-c", "import sys; sys.path.insert(0, sys.argv[1]); import yt_dlp; yt_dlp.main()", pkg.to_str().unwrap()]);
        for (k, v) in env(&control) {
            c.env(k, v);
        }
        let out = retrying(|| c.output());
        assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
        let seen: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&control).unwrap()).unwrap();
        assert_eq!(seen["connect"], serde_json::json!(["ok", null]));
        assert_eq!(seen["connect_ex"], serde_json::json!(["ok", 0]));
        assert_eq!(seen["other_family"], serde_json::json!(["ok", null]));
        assert!(tcp.accept().is_ok());
        assert!(udp.recv(&mut [0u8; 8]).is_ok());
    }

    /// Runs the wrapper with `args`, the fake printing `json` (exactly) and
    /// exiting with `codes`: the exit code, stdout and stderr.
    fn answer(json: &str, args: &[&str], codes: &str) -> (i32, String, String) {
        let r = rig(None, false);
        let file = r.dir.path().join("answer.json");
        std::fs::write(&file, json).unwrap();
        let mut c = command(&r, codes, args, &[("FAKE_OUTFILE_1", file.to_str().unwrap())]);
        let out = retrying(|| c.output());
        (out.status.code().unwrap(), String::from_utf8(out.stdout).unwrap(), String::from_utf8(out.stderr).unwrap())
    }

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)).unwrap()
    }

    /// The real capture with formats (`tests/fixtures/video-formats.json`,
    /// see the README there), each of its two hosts replaced by a public
    /// literal: no test resolves a name but `localhost`.
    fn captured() -> String {
        fixture("video-formats.json")
            .replace("rr4---sn-aaa-aaaaa.googlevideo.com", "142.250.74.14")
            .replace("manifest.googlevideo.com", "[2a00:1450:4007:80e::200e]")
    }

    fn with_host(url: &serde_json::Value, host: &str) -> serde_json::Value {
        let mut u = url::Url::parse(url.as_str().unwrap()).unwrap();
        u.set_host(Some(host)).unwrap();
        u.to_string().into()
    }

    fn ids(v: &serde_json::Value) -> Vec<&str> {
        v["formats"].as_array().unwrap().iter().map(|f| f["format_id"].as_str().unwrap()).collect()
    }

    fn parsed(out: &str, err: &str) -> serde_json::Value {
        serde_json::from_str(out).unwrap_or_else(|e| panic!("{e}: {out:?} / {err}"))
    }

    #[test]
    fn formats_that_reach_the_local_network_are_dropped_from_a_real_answer() {
        let text = captured();
        assert!(!text.contains("googlevideo.com"), "every host is a literal");
        // Untouched, the answer is printed byte for byte.
        let (code, out, err) = answer(&text, &["--no-warnings", "-J", "--flat-playlist", "--", "URL"], "0");
        assert_eq!(code, 0, "{err}");
        assert!(out == text, "the answer was rewritten");

        let mut v: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(ids(&v), ["233", "234", "139", "140", "251", "160"]);
        // 233: only its manifest is local, and one local address is enough.
        v["formats"][0]["manifest_url"] = with_host(&v["formats"][0]["manifest_url"], "192.168.1.10");
        // 139: by a name that resolves locally.
        v["formats"][2]["url"] = with_host(&v["formats"][2]["url"], "localhost");
        let (code, out, err) = answer(&v.to_string(), &["-J"], "0");
        assert_eq!(code, 0, "{err}");
        let got = parsed(&out, &err);
        assert_eq!(ids(&got), ["234", "140", "251", "160"]);
        assert_eq!((&got["format_id"], &got["url"]), (&v["format_id"], &v["url"]), "the chosen format stays");
        assert_eq!(got["title"], "Me at the zoo");
    }

    #[test]
    fn a_live_answer_fails_and_prints_nothing() {
        let base: serde_json::Value = serde_json::from_str(&captured()).unwrap();
        for (field, value) in [
            ("is_live", serde_json::Value::from(true)),
            ("live_status", "is_live".into()),
            ("live_status", "is_upcoming".into()),
        ] {
            let mut v = base.clone();
            v[field] = value.clone();
            let (code, out, err) = answer(&v.to_string(), &["-J", "--", "URL"], "0");
            assert_eq!((code, out.as_str()), (1, ""), "{field} {value}: {err}");
            assert_eq!(err.trim_end().lines().last(), Some(crate::ytdlp::LIVE_REFUSAL), "{field} {value}");
        }
        // The real capture says `is_live: false, live_status: not_live`;
        // `was_live` plays too.
        for (field, value) in [("is_live", serde_json::Value::from(false)), ("live_status", "was_live".into())] {
            let mut v = base.clone();
            v[field] = value;
            let (code, _, err) = answer(&v.to_string(), &["-J", "--", "URL"], "0");
            assert_eq!(code, 0, "{err}");
        }
    }

    #[test]
    fn an_answer_whose_every_format_is_local_fails_and_prints_nothing() {
        let text = captured().replace("142.250.74.14", "10.0.0.1").replace("[2a00:1450:4007:80e::200e]", "localhost");
        for flag in ["-J", "--dump-single-json"] {
            let (code, out, err) = answer(&text, &[flag, "--", "URL"], "0");
            assert_ne!(code, 0, "{flag}");
            assert_eq!(out, "", "{flag}");
            assert!(err.contains("every format points to the local network"), "{flag}: {err}");
        }
    }

    #[test]
    fn what_mpv_would_play_is_judged_the_chosen_format_and_the_requested_ones() {
        let base: serde_json::Value = serde_json::from_str(&captured()).unwrap();
        // The chosen format (the top level, what mpv plays) local: nothing
        // is left to play, even with public formats around.
        let mut v = base.clone();
        v["url"] = with_host(&v["url"], "127.0.0.1");
        let (code, out, err) = answer(&v.to_string(), &["-J"], "0");
        assert_eq!((code, out.as_str()), (1, ""), "{err}");
        assert!(err.contains("every format points to the local network"), "{err}");

        // Requested formats (a merge): one local, and the merge is dropped;
        // the chosen format's own address is left to play.
        let mut v = base.clone();
        let mut local = v["formats"][5].clone();
        local["url"] = with_host(&local["url"], "10.0.0.1");
        v["requested_formats"] = serde_json::json!([v["formats"][3].clone(), local]);
        let (code, out, err) = answer(&v.to_string(), &["-J"], "0");
        assert_eq!(code, 0, "{err}");
        let got = parsed(&out, &err);
        assert!(got.get("requested_formats").is_none(), "{}", got["requested_formats"]);
        assert_eq!(got["url"], v["url"]);
        assert_eq!(ids(&got).len(), 6, "the formats themselves are public");
        // All public: kept.
        let mut v = base.clone();
        v["requested_formats"] = serde_json::json!([v["formats"][3].clone(), v["formats"][5].clone()]);
        let sent = v.to_string();
        let (code, out, err) = answer(&sent, &["-J"], "0");
        assert_eq!(code, 0, "{err}");
        assert!(out == sent, "an answer with nothing local is printed as it came");
        // Nothing but a merge, and the merge local: nothing left.
        let mut v = base.clone();
        let mut local = v["formats"][3].clone();
        local["url"] = with_host(&local["url"], "172.16.0.1");
        v["requested_formats"] = serde_json::json!([local]);
        v.as_object_mut().unwrap().remove("url");
        let (code, out, err) = answer(&v.to_string(), &["-J"], "0");
        assert_eq!((code, out.as_str()), (1, ""), "{err}");
    }

    #[test]
    fn every_address_a_format_or_a_subtitle_is_fetched_from_is_judged() {
        let mut v: serde_json::Value = serde_json::from_str(&captured()).unwrap();
        // 234: fragments, one of them local.
        v["formats"][1]["fragment_base_url"] = "https://8.8.8.8/".into();
        v["formats"][1]["fragments"] = serde_json::json!([{"url": "https://8.8.8.8/s1"}, {"url": "http://[fd00::1]/s2"}]);
        // 140: a local fragment base.
        v["formats"][3]["fragment_base_url"] = "https://169.254.169.254/".into();
        // 160: an address without a host (a file of the device).
        v["formats"][5]["url"] = "file:///etc/passwd".into();
        // 139: fragments, all public: kept.
        v["formats"][2]["fragments"] = serde_json::json!([{"url": "https://8.8.8.8/s1"}]);
        v["requested_subtitles"] = serde_json::json!({
            "en": {"ext": "vtt", "url": "http://127.0.0.1/en.vtt"},
            "fr": {"ext": "vtt", "url": "https://8.8.8.8/fr.vtt"},
        });
        // 233: fragment paths after a public base, joined as mpv joins
        // them (plain concatenation): a host of its own is not hidden in a
        // path.
        v["formats"][0]["fragment_base_url"] = "https://8.8.8.8/".into();
        v["formats"][0]["fragments"] = serde_json::json!([{"path": "seg/1"}, {"path": "seg/2"}]);
        let mut hidden = v.clone();
        hidden["formats"][0]["fragment_base_url"] = "https://8.8.8.8".into();
        hidden["formats"][0]["fragments"] = serde_json::json!([{"path": "seg/1"}, {"path": "@192.168.0.1/x"}]);
        let (code, out, err) = answer(&v.to_string(), &["-J"], "0");
        assert_eq!(code, 0, "{err}");
        let got = parsed(&out, &err);
        assert_eq!(ids(&got), ["233", "139", "251"]);
        assert_eq!(got["requested_subtitles"], serde_json::json!({"fr": {"ext": "vtt", "url": "https://8.8.8.8/fr.vtt"}}));
        let (code, out, err) = answer(&hidden.to_string(), &["-J"], "0");
        assert_eq!(code, 0, "{err}");
        assert_eq!(ids(&parsed(&out, &err)), ["139", "251"], "https://8.8.8.8@192.168.0.1/x is 192.168.0.1");
        // A path with no base to join it to cannot be judged: dropped.
        let no_base = serde_json::json!({"id": "x", "url": "https://8.8.8.8/b", "formats": [
            {"format_id": "a", "url": "https://8.8.8.8/a", "fragments": [{"path": "seg/1"}]},
            {"format_id": "b", "url": "https://8.8.8.8/b"},
        ]});
        let (code, out, err) = answer(&no_base.to_string(), &["-J"], "0");
        assert_eq!(code, 0, "{err}");
        assert_eq!(ids(&parsed(&out, &err)), ["b"]);
    }

    #[test]
    fn a_flat_playlists_entries_are_left_and_a_full_playlists_local_entries_dropped() {
        // The real flat capture, whose entries name youtube.com: printed
        // byte for byte (and nothing resolved: the tests have no network).
        let text = fixture("playlist.json");
        let (code, out, err) = answer(&text, &["-J", "--flat-playlist"], "0");
        assert_eq!(code, 0, "{err}");
        assert!(out == text, "the flat playlist was rewritten");

        let video: serde_json::Value = serde_json::from_str(&captured()).unwrap();
        let mut local = video.clone();
        local["id"] = "local".into();
        local["url"] = with_host(&local["url"], "192.168.0.2");
        // A flat entry is left even when it names a local address: this
        // filter does not judge page addresses (the module's residual risk).
        // Local here so that judging it would drop it whatever the network.
        let flat = serde_json::json!({"_type": "url", "url": "http://192.168.1.1/page"});
        let list = serde_json::json!({"_type": "playlist", "id": "PL", "entries": [local, video, flat]});
        let (code, out, err) = answer(&list.to_string(), &["-J"], "0");
        assert_eq!(code, 0, "{err}");
        let got = parsed(&out, &err);
        let entries = got["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0]["id"], "jNQXAC9IVRw");
        assert_eq!(entries[1], flat);
        // Every full entry local: nothing left to play.
        let list = serde_json::json!({"_type": "playlist", "id": "PL", "entries": [list["entries"][0].clone()]});
        let (code, out, _) = answer(&list.to_string(), &["-J"], "0");
        assert_eq!((code, out.as_str()), (1, ""));
    }

    /// A fake `yt_dlp` whose `main` tries to import `curl_cffi` (a fake one
    /// sits on the path, next to the package) and records it with what the
    /// interpreter has of `site`.
    const EXTENSIONS_MAIN: &str = r#"import json, os, sys


def main(argv=None):
    try:
        import curl_cffi
        imported = True
    except ImportError:
        imported = False
    with open(os.environ["FAKE_RESULT"], "w") as f:
        json.dump({
            "curl_cffi": imported,
            "no_site": sys.flags.no_site,
            "site": "site" in sys.modules,
            "site_packages": [p for p in sys.path if p.endswith("-packages")],
        }, f)
    raise SystemExit(0)
"#;

    /// Nothing that connects outside Python's `socket` gets in: `curl_cffi`
    /// is not imported even when it is on the path, and no `site` runs (so
    /// the system's site-packages are not on the path). The control: the same
    /// package under a plain `python3 -I` imports the fake and has `site`.
    #[test]
    fn no_c_extension_and_no_site_packages_get_past_the_guard() {
        let r = rig_with(None, false, EXTENSIONS_MAIN);
        let fake = r.dir.path().join(PACKAGE_DIR).join("curl_cffi");
        std::fs::create_dir_all(&fake).unwrap();
        let mark = r.dir.path().join("curl_cffi-imported");
        std::fs::write(fake.join("__init__.py"), format!("open({:?}, 'w').close()\n", mark.to_str().unwrap())).unwrap();
        let result = r.dir.path().join("result.json");
        let (code, _, _) = run_with(&r, "0", &[], &[("FAKE_RESULT", result.to_str().unwrap())]);
        assert_eq!(code, 0);
        let seen: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&result).unwrap()).unwrap();
        assert_eq!(seen["curl_cffi"], false, "{seen}");
        assert!(!mark.exists(), "the fake curl_cffi ran");
        assert_eq!((&seen["no_site"], &seen["site"], &seen["site_packages"]), (&1.into(), &false.into(), &serde_json::json!([])), "{seen}");

        let pkg = r.dir.path().join(PACKAGE_DIR);
        let out = Command::new(real_python3())
            .args(["-I", "-c", "import sys; sys.path.insert(0, sys.argv[1]); import yt_dlp; yt_dlp.main()", pkg.to_str().unwrap()])
            .env("FAKE_RESULT", &result)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
        let seen: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&result).unwrap()).unwrap();
        assert_eq!((&seen["curl_cffi"], &seen["no_site"], &seen["site"]), (&true.into(), &0.into(), &true.into()), "{seen}");
        assert!(mark.exists());
    }

    /// A fake `yt_dlp` that answers `FAKE_ANSWER` after replacing the
    /// process's resolver with one that knows three names, no network
    /// needed: `public.example` (public), `rebind.example` (one public and
    /// one local address) and `slow.example` (public, after 8 s).
    const DNS_MAIN: &str = r#"import os, socket, sys, time

_real = socket.getaddrinfo


def _answer(*ips):
    return [(socket.AF_INET, socket.SOCK_STREAM, 6, "", (ip, 0)) for ip in ips]


def fake(host, *args, **kwargs):
    if host == "public.example":
        return _answer("8.8.4.4")
    if host == "rebind.example":
        return _answer("8.8.8.8", "192.168.1.1")
    if host == "slow.example":
        time.sleep(8)
        return _answer("8.8.4.4")
    return _real(host, *args, **kwargs)


def main(argv=None):
    socket.getaddrinfo = fake
    sys.stdout.buffer.write(os.environ["FAKE_ANSWER"].encode())
    sys.stdout.flush()
    raise SystemExit(0)
"#;

    #[test]
    fn a_host_with_one_local_address_or_none_in_time_is_dropped() {
        let doc = serde_json::json!({"id": "x", "url": "https://public.example/a", "formats": [
            {"format_id": "public", "url": "https://public.example/a"},
            {"format_id": "rebind", "url": "https://rebind.example/a"},
            {"format_id": "slow", "url": "https://slow.example/a"},
        ]});
        let r = rig_with(None, false, DNS_MAIN);
        let doc_s = doc.to_string();
        let mut c = command(&r, "0", &["-J"], &[("FAKE_ANSWER", &doc_s)]);
        let started = std::time::Instant::now();
        let out = retrying(|| c.output());
        let took = started.elapsed().as_secs_f64();
        let (stdout, stderr) = (String::from_utf8(out.stdout).unwrap(), String::from_utf8_lossy(&out.stderr).to_string());
        assert_eq!(out.status.code(), Some(0), "{stderr}");
        assert_eq!(ids(&parsed(&stdout, &stderr)), ["public"]);
        // The slow name was waited for until the deadline, not to its end.
        assert!((RESOLVE_SECONDS - 0.5..RESOLVE_SECONDS + 2.5).contains(&took), "{took} s");
    }

    #[test]
    fn only_a_json_answer_is_judged_and_an_unreadable_one_is_not_passed_on() {
        let local = r#"{"id": "x", "url": "http://127.0.0.1/a.m4a"}"#;
        assert_eq!(answer(local, &["-J"], "0").0, 1);
        // Formats and no chosen one: every format local is a failure too.
        let formats_only = r#"{"id": "x", "formats": [{"format_id": "a", "url": "http://10.0.0.1/a"}]}"#;
        assert_eq!(answer(formats_only, &["-J"], "0").0, 1);
        // Nothing to fetch at all: nothing to judge.
        assert_eq!(answer(r#"{"id": "x"}"#, &["-J"], "0"), (0, r#"{"id": "x"}"#.to_string(), String::new()));
        // No -J, or a -J that is the URL after `--`: passed on as it comes.
        assert_eq!(answer(local, &["--print", "x"], "0"), (0, local.to_string(), String::new()));
        assert_eq!(answer(local, &["--", "-J"], "0"), (0, local.to_string(), String::new()));
        // A group of short options holding J is a -J.
        for group in ["-qJ", "-Jq", "-vqJs"] {
            assert_eq!(answer(local, &[group], "0").0, 1, "{group}");
        }
        // A J that is the value of an option taking one is not: inside the
        // group (-oJ: output template "J"), or as the next argument.
        let passed = (0, local.to_string(), String::new());
        assert_eq!(answer(local, &["-oJ"], "0"), passed);
        assert_eq!(answer(local, &["-qo", "-J"], "0"), passed);
        assert_eq!(answer(local, &["-f", "-J"], "0"), passed);
        assert_eq!(answer(local, &["-f", "x", "-J"], "0").0, 1, "after the value, options again");
        // A -J answer that is not JSON: nothing is printed, and a success
        // becomes a failure; a failure keeps its own code.
        let (code, out, err) = answer("{\"id\": ", &["-J"], "0");
        assert_eq!((code, out.as_str()), (1, ""));
        assert!(err.contains("not JSON"), "{err}");
        assert_eq!(answer("{\"id\": ", &["-J"], "2").0, 2);
        // Several answers (several URLs): each is judged.
        let two = format!("{local}\n{}\n", r#"{"id": "y", "url": "https://8.8.8.8/a.m4a"}"#);
        assert_eq!(answer(&two, &["-J"], "0").0, 1);
    }

    /// `reason::coded` recognises the guard's refusal by this text: the two
    /// must not drift apart.
    #[test]
    fn the_guards_refusal_is_the_text_the_page_codes() {
        assert!(GUARD.contains(&format!("\"{}%s\"", crate::reason::LOCAL_REFUSED)));
    }

    /// What mpv's hook asks (`ytdl_hook.lua` of mpv 0.37).
    const MPV_ARGS: [&str; 11] = [
        "--no-warnings", "-J", "--flat-playlist", "--sub-format", "ass/srt/best", "--format", "bestaudio/best",
        "--all-subs", "--no-playlist", "--", "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
    ];

    /// One run of the wrapper as `env` says (mpv's when it lacks
    /// `OWN_RUN_ENV`), and the failure it recorded, if any, with its stderr.
    fn recorded(args: &[&str], codes: &str, env: &[(&str, &str)]) -> (Option<crate::playfail::PlaybackFailure>, String) {
        let r = rig(None, false);
        let mut c = command(&r, codes, args, env);
        let out = retrying(|| c.output());
        (crate::playfail::PlaybackFailure::load(r.dir.path()), String::from_utf8(out.stderr).unwrap())
    }

    /// A failed run served for mpv is recorded with yt-dlp's last line,
    /// which still reaches mpv's log; one of the plugin's own, a success, or
    /// a run that is not a `-J` (a download) is not.
    #[test]
    fn a_failed_run_for_mpv_is_recorded_and_its_words_still_reach_mpv() {
        let said = "WARNING: [youtube] something\nERROR: [youtube] dQw4w9WgXcQ: Video unavailable\n";
        let before = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64();
        let (f, err) = recorded(&MPV_ARGS, "1", &[("FAKE_ERR_1", said), ("FAKE_OUT_1", "null\n")]);
        let f = f.expect("recorded");
        assert_eq!(f.code, crate::playfail::YTDLP_FAILED);
        assert_eq!(f.host, "www.youtube.com");
        assert_eq!(f.detail, "ERROR: [youtube] dQw4w9WgXcQ: Video unavailable");
        assert!(f.time >= before - 1.0, "{} < {before}", f.time);
        assert_eq!(err, said, "stderr passed on whole");

        let own = [("FAKE_ERR_1", said), (crate::ytdlp::OWN_RUN_ENV, "1")];
        assert_eq!(recorded(&MPV_ARGS, "1", &own).0, None, "the plugin's own run");
        assert_eq!(recorded(&MPV_ARGS, "0", &[("FAKE_OUT_1", "{\"id\": \"x\"}\n")]).0, None, "a success");
        assert_eq!(recorded(&["-o", "x", "--", "https://a.example/"], "1", &[("FAKE_ERR_1", said)]).0, None, "not a -J");
    }

    /// One code per kind of failure, each from what tells it.
    #[test]
    fn each_kind_of_failure_gets_its_code() {
        let code = |codes: &str, env: &[(&str, &str)]| recorded(&MPV_ARGS, codes, env).0.expect("recorded").code;
        // The answer's every format is local: the guard's own refusal.
        let local = r#"{"id": "x", "url": "http://127.0.0.1/a.m4a"}"#;
        assert_eq!(code("0", &[("FAKE_OUT_1", local)]), crate::playfail::GUARD_REFUSED);
        // The socket guard, in yt-dlp's words.
        let refused = "ERROR: [generic] Unable to download webpage: [Errno 13] local network refused: 192.168.1.1\n";
        assert_eq!(code("1", &[("FAKE_ERR_1", refused)]), crate::playfail::GUARD_REFUSED);
        // A live answer.
        assert_eq!(code("0", &[("FAKE_OUT_1", r#"{"id": "x", "is_live": true}"#)]), crate::playfail::LIVE_REFUSED);
        // Nothing to play, as yt-dlp says it.
        let none = "ERROR: [youtube] x: Requested format is not available. Use --list-formats\n";
        assert_eq!(code("1", &[("FAKE_ERR_1", none)]), crate::playfail::NO_FORMATS);
        let none = "ERROR: [generic] No video formats found!\n";
        assert_eq!(code("1", &[("FAKE_ERR_1", none)]), crate::playfail::NO_FORMATS);
        // Anything else, and a failure that said nothing.
        assert_eq!(code("1", &[]), crate::playfail::YTDLP_FAILED);
        let (f, _) = recorded(&MPV_ARGS, "1", &[]);
        assert_eq!(f.unwrap().detail, "");
    }
}

# `ritornello-plugin-nrj-metas`

A `metadata` plugin (see [docs/plugins.md](../../docs/plugins.md)) that shows
what is playing on the NRJ group's webradios: NRJ, Nostalgie, Chérie FM and
Rire & Chansons.

**Why it exists.** The group's streams do emit ICY, but what they put in it is
an internal cart code, duplicated on both sides of the separator —
`DD25-19 - DD25-19` on Rire & Chansons, `5612 - 5612` on NRJ. There is nothing
to split. This is worse than an empty ICY: it *looks* like an ordinary
`Artist - Title` pair, so left alone it gets displayed as one, wrongly. Each
brand site, on the other hand, exposes its stations' on-air data without
authentication, with artist and title already separated.

**Nothing to configure.** The table is embedded in the binary
(`src/stations.toml`). The optional `/etc/ritornello/nrj-metas.toml`
(variable `RITORNELLO_NRJ_METAS`, example in `deploy/`) is only there to fix
an entry gone stale or add one without recompiling; its entries are read
*before* the embedded table.

## Four brands, four hosts, 365 stations

| Brand | Host | Stations |
|---|---|---|
| NRJ | nrj.fr | 191 |
| Nostalgie | nostalgie.fr | 92 |
| Chérie FM | cheriefm.fr | 57 |
| Rire & Chansons | rireetchansons.fr | 25 |

No station is listed under two brands, and **a host only serves its own
stations**: measured, asking `rireetchansons.fr` for NRJ's id 158 returns
nothing. The brand is therefore not decoration in the table — it is what
tells the plugin which host to query, and a wrong one means silence rather
than a wrong station's titles.

Recognition is based on a **token** of the stream URL — the last segment,
bordered on both sides by a non-alphanumeric character, never a raw
substring — the same rule `radiofrance-metas` needed, for the same reason: a
token must not be recognized as a fragment of a longer one NRJ might mint
later.

## Chérie FM's 403

`cheriefm.fr` answers `403` to a request without a full browser
`User-Agent` — measured with `curl` too, so it is the header the endpoint
checks, not the rate of requests. It goes further than that: measured the
same minute, on the same machine, Node's own HTTP stack (`fetch`, and the raw
`https.request` underneath it) gets a `403` from `cheriefm.fr`
**regardless of headers**, while the other three hosts answer `200` without
complaint. `curl` with the same browser `User-Agent`, on the other hand, gets
`200` from all four, Chérie FM included. Two independent facts, not one:
Chérie FM wants a real `User-Agent` (true for both clients), and it
separately refuses Node's HTTP stack outright (true regardless of headers) —
most likely a TLS/HTTP client fingerprint its front end blocks. This is why
`scripts/fetch-stations.mjs` shells out to `curl` rather than using `fetch`: a
future "modernization" back to `fetch` would silently drop a quarter of the
table with no error, since the other three brands would keep working. The
plugin's own runtime queries with a full browser `User-Agent` for the same
reason (see `live::follows`).

## The rhythm: the cart code is the trigger, the deadline is only a net

Unlike Radio France, which tells the plugin itself when to call back again,
NRJ's endpoint answers a snapshot and an `end_timestamp` it is **measurably
late on** — 40 to 70 seconds past its own deadline, measured. Polling on that
deadline alone would mean querying for the *previous* item most of the time.

What the plugin has instead, and uses as the real trigger, is the ICY cart
code the core already hands it in every frame (`Known.stream_title`).
Measured twice: the cart code changes *before* the JSON does, and the JSON
caught up within 33 seconds both times. So the code says *when*, the
endpoint says *what*, and the announced deadline is kept only as a safety
net — for a station whose cart code stops moving, not for the ordinary
case.

**A 30-second advert plays on connection**, and it is invisible to this
plugin, and to every other `metadata` plugin: the stream's `StreamTitle` is
blank during it, and the core's own ICY parsing (`icy_title` in
`ritornello-core`) discards a blank one rather than reporting it, so
`Known.stream_title` never carries anything for the ad to trigger on.
Measured, the first real cart code arrived only at t+61 s. This is exactly
why the first query is triggered by **recognizing the station**, immediately,
rather than by waiting for a change of code that a blank ICY can never
produce.

**A four-second jingle aired between two tracks on NRJ**, measured — which is
why a debounce exists at all: a poke restarts a short wait, so anything
shorter than the delay never causes a request, clearing a jingle without
approaching the server's own lag.

## The filler rule

An announced artist equal to the station's own name is filler, not a track:
measured on 4 stations out of 365 at one instant. But the rule is not a rare
edge case everywhere — a 30-sample poll of Nostalgie's main station found 24
fillers to 6 real tracks. On some stations filler is the ordinary case, not
the exception.

## The wallpaper-cover rule, independent of the above

An announced image that is the station's own wallpaper (its filename
containing `default`) is not this track's cover: measured on 6 stations out
of 365. This rule is checked **independently** of the filler rule above —
**4 of those 6 stations announced a perfectly valid artist at the same
moment**. Conflating the two would have classified real tracks as filler
merely because the station had not yet set a per-track image.

## `musicbrainz`'s ICY probe needs no special case here

`musicbrainz`, declared after this plugin (see
`deploy/plugins.example.toml`), also probes any station's raw ICY string to
learn whether it can be split. On an NRJ station its candidate is exactly the
cart code duplicated across the separator — `"DD25-19"` on both sides, say —
which it validates against a recording search. That search has nothing to
find an artist named `"DD25-19"` against, so the candidate never validates.

This was checked against `musicbrainz`'s own anchoring rule
(`PROBES_BEFORE_ANCHORING = 5`, see `ritornello-plugin-musicbrainz`) rather
than assumed: a new cart code is a new *splittable string*, so each distinct
one this plugin sees would reopen the question once — but after five such
probes have all concluded "do not split", the verdict anchors and nothing
reprobes it automatically. The cost of this whole interaction is therefore
bounded at twenty MusicBrainz requests, ever, per NRJ station, whether or not
`nrj-metas` is even installed. No code was added here to special-case it: the
existing anchoring rule already absorbs a station whose ICY never carries a
real title.

## Regenerating the table

    node scripts/fetch-stations.mjs

Rewrites `src/stations.toml` from the four brands' own `/onair.json`
endpoints. With `--verifier` it writes nothing and exits nonzero if the
committed table has drifted from those sources.

# `ritornello-plugin-radiofrance-metas`

A `metadata` plugin (see [docs/plugins.md](../../docs/plugins.md)) that shows
what is playing on Radio France's stations.

**Why it exists.** Radio France's streams carry *no ICY metadata at all* — no
`icy-metaint` header, not even the filler text OUI FM emits. Without this
plugin, a Radio France station on the device shows nothing but its name. The
station's *live* endpoint, on the other hand, answers without authentication,
states itself when to be called again, and — on the music stations — gives
title and artist already separated.

**Nothing to configure.** The table of the 74 stations is embedded in the
binary (`src/stations.toml`). The optional
`/etc/ritornello/radiofrance-metas.toml` (variable
`RITORNELLO_RADIOFRANCE_METAS`, example in `deploy/`) is only there to fix an
entry gone stale or add one without recompiling; its entries are read *before*
the embedded table.

## How a station is recognized

By the **mount** of its stream, matched as a *token* of the configured URL —
bordered on both sides by a non-alphanumeric character, never as a raw
substring. `fip` is a prefix of `fipgroove` and `francemusique` of
`francemusiquebaroque`: a plain substring search would have let the first entry
capture the others and display the wrong station's titles, with no sign of
error.

One entry therefore covers every form the same station is served under:

    https://icecast.radiofrance.fr/fipgroove-midfi.mp3
    https://icecast.radiofrance.fr/fipgroove-hifi.aac
    https://direct.fipradio.fr/live/fipgroove-midfi.mp3      (historical name)
    https://stream.radiofrance.fr/fipgroove/fipgroove.m3u8   (HLS)

## The two rendering profiles

The last segment of the live URL is not the station but a **rendering
profile**, and it changes what comes back — the wrong one makes the plugin
silent. Measured on Mouv' at one instant: `webrf_fip_player` answered
`Le direct / Mouv'` (the station's baseline) while `webrf_mouv_player` answered
`La Playlist / SOOLKING - Bye Bye (feat. TAYC)`, which was what actually aired.
Each station therefore carries its profile in the table.

| Profile | Shape of the answer | Stations |
|---|---|---|
| `webrf_fip_player` | the **song** object: title and artist already separated, and the time window is the song's, so its duration is reported | FIP, its 12 webradios, France Musique's 11 |
| `webrf_mouv_player` | the **programme** object: the programme name, plus what is playing inside it as a single `ARTIST - Title` string. The window is the programme's, so no duration is reported | the 5 other national brands, the 45 local stations |

The second profile's name is incidental — it is a server-side profile, not a
Mouv' endpoint. It is used wherever it is the only one that surfaces the
current song (Mouv', France Musique, the local stations); on purely spoken
stations it returns the same programme/detail pair as the brand's own profile
(checked on France Inter, franceinfo and France Culture).

## The album, and why it is a second request

Neither profile carries an album. It lives in a different endpoint — the
station's *schedule* (`livemeta/pull/<id>`), where every past and present item
is listed with its `titreAlbum`. The current song is found there by its
identifier: the live answer's `songUuid` matches a schedule entry's `songId`
(**not** its `uuid`, which identifies the schedule entry rather than the song —
confusing the two would silently find nothing).

The schedule is queried **once per track**, not once per poll: over the life of
one song the answer would not change. So the extra cost is one request per
track, never per refresh.

It is best-effort by nature. The schedule is frequently **one track behind** —
measured, its last entry ends exactly when the current song starts — and on
some stations it never catches up within a track. When the song is not there,
the enrichment simply goes out without an album; nothing else is held back or
delayed. To avoid doubling the requests made to a third party for an answer
that does not come, the plugin **stops asking a station's schedule after five
consecutive tracks without an album**, until that station is selected again.

## Known limitations

- **The live endpoint is private and undocumented.** Only the *list* of
  stations is published. It may change, start requiring authentication, or
  disappear without notice. Its failure is silent on screen and never delays
  playback, and reconnection backs off progressively.
- **The album is often absent**, and that is the endpoint's doing, not a bug:
  the schedule it is read from usually lags one track behind. Observed on a
  single sweep: present on FIP and most of its webradios, on France Musique's,
  on Mouv' and on France Inter; absent on the local stations. Expect it to come
  and go on the same station.
- **Three webradios are not covered**, although their streams exist:
  `franceinter_la_musique_inter` (mount `franceinterlamusiqueinter`),
  `francebleu_chanson_francaise` (mount `fbchansonfrancaise`) and
  `francebleu_annee_80` (no Icecast mount found). They answer on the site's
  GraphQL path but have **no live-endpoint identifier**: 1 095 identifiers were
  scanned without a single hit. Adding them would mean a second, far more
  fragile data path, so they are left out rather than guessed.
- **The national `ICI` feed (identifier 56) is not covered either**: its
  identifier is known, but no Icecast mount was found for it, so there is
  nothing to recognize a URL by.
- **One pairing rests on elimination**: identifier 407 ("Films") with the mount
  `francemusiquelabo` — read as *la B.O.*, film scores. Every other France
  Musique webradio was paired first, leaving exactly one mount and exactly one
  station. It is the first entry to suspect if a wrong title ever shows up.
- **Local stations are labelled "France Bleu"**, not "ici", because that is how
  Radio France's own documentation still names them. The label only ever
  appears in the logs.
- **Mouv' has no sub-webradios.** Not an omission: no `mouv*` mount answers, no
  `mouv_*` brand slug is accepted, and the site declares `webradios = ["mouv"]`.
- Outside music, the programme name and its detail are displayed rather than
  nothing — there is no ICY to fall back on, so the alternative is a blank
  line.

## Regenerating the table

    node crates/ritornello-plugin-radiofrance-metas/scripts/fetch-stations.mjs

It rewrites `src/stations.toml` **and the table below** from Radio France's own
published sources: the Open API documentation, whose example responses pair
each station's `liveStream` (hence its mount) with its `playerUrl` (which
carries `id_station=<n>`), and — for the 13 stations that documentation does
not list — the site's own webradio cards, each of whose mounts is re-checked
against the Icecast server on every run. With `--verifier` it writes nothing
and exits nonzero if the committed files have drifted from those sources.

## Stations covered

<!-- stations:auto:début — généré par scripts/fetch-stations.mjs, ne pas éditer à la main -->
| Station | Mount | Id | Profile |
|---|---|---|---|
| **Les six marques nationales** | | | |
| France Inter | `franceinter` | 1 | `webrf_mouv_player` |
| franceinfo | `franceinfo` | 2 | `webrf_mouv_player` |
| France Musique | `francemusique` | 4 | `webrf_mouv_player` |
| France Culture | `franceculture` | 5 | `webrf_mouv_player` |
| Mouv' | `mouv` | 6 | `webrf_mouv_player` |
| FIP | `fip` | 7 | `webrf_fip_player` |
| **Webradios FIP** | | | |
| FIP Rock | `fiprock` | 64 | `webrf_fip_player` |
| FIP Jazz | `fipjazz` | 65 | `webrf_fip_player` |
| FIP Groove | `fipgroove` | 66 | `webrf_fip_player` |
| FIP Monde | `fipworld` | 69 | `webrf_fip_player` |
| FIP Nouveautés | `fipnouveautes` | 70 | `webrf_fip_player` |
| FIP Reggae | `fipreggae` | 71 | `webrf_fip_player` |
| FIP Electro | `fipelectro` | 74 | `webrf_fip_player` |
| FIP Metal | `fipmetal` | 77 | `webrf_fip_player` |
| FIP Pop | `fippop` | 78 | `webrf_fip_player` |
| FIP Hip-Hop | `fiphiphop` | 95 | `webrf_fip_player` |
| FIP Sacré français ! | `fipsacrefrancais` | 96 | `webrf_fip_player` |
| FIP Cultes | `fipcultes` | 709 | `webrf_fip_player` |
| **Webradios France Musique** | | | |
| France Musique Classique Easy | `francemusiqueeasyclassique` | 401 | `webrf_fip_player` |
| France Musique Classique Plus | `francemusiqueclassiqueplus` | 402 | `webrf_fip_player` |
| France Musique Concerts Radio France | `francemusiqueconcertsradiofrance` | 403 | `webrf_fip_player` |
| France Musique Ocora | `francemusiqueocoramonde` | 404 | `webrf_fip_player` |
| France Musique Jazz | `francemusiquelajazz` | 405 | `webrf_fip_player` |
| France Musique Contemporaine | `francemusiquelacontemporaine` | 406 | `webrf_fip_player` |
| France Musique Films | `francemusiquelabo` | 407 | `webrf_fip_player` |
| France Musique Baroque | `francemusiquebaroque` | 408 | `webrf_fip_player` |
| France Musique Opéra | `francemusiqueopera` | 409 | `webrf_fip_player` |
| France Musique Piano Zen | `francemusiquepianozen` | 410 | `webrf_fip_player` |
| France Musique Classique Love | `francemusiqueclassiquelove` | 411 | `webrf_fip_player` |
| **Les 45 locales ici (ex-France Bleu)** | | | |
| France Bleu RCFM | `fbfrequenzamora` | 11 | `webrf_mouv_player` |
| France Bleu Alsace | `fbalsace` | 12 | `webrf_mouv_player` |
| France Bleu Armorique | `fbarmorique` | 13 | `webrf_mouv_player` |
| France Bleu Auxerre | `fbauxerre` | 14 | `webrf_mouv_player` |
| France Bleu Béarn Bigorre | `fbbearn` | 15 | `webrf_mouv_player` |
| France Bleu Belfort-Montbéliard | `fbbelfort` | 16 | `webrf_mouv_player` |
| France Bleu Berry | `fbberry` | 17 | `webrf_mouv_player` |
| France Bleu Besançon | `fbbesancon` | 18 | `webrf_mouv_player` |
| France Bleu Bourgogne | `fbbourgogne` | 19 | `webrf_mouv_player` |
| France Bleu Breizh Izel | `fbbreizizel` | 20 | `webrf_mouv_player` |
| France Bleu Champagne-Ardenne | `fbchampagne` | 21 | `webrf_mouv_player` |
| France Bleu Cotentin | `fbcotentin` | 22 | `webrf_mouv_player` |
| France Bleu Creuse | `fbcreuse` | 23 | `webrf_mouv_player` |
| France Bleu Drôme Ardèche | `fbdromeardeche` | 24 | `webrf_mouv_player` |
| France Bleu Gard Lozère | `fbgardlozere` | 25 | `webrf_mouv_player` |
| France Bleu Gascogne | `fbgascogne` | 26 | `webrf_mouv_player` |
| France Bleu Gironde | `fbgironde` | 27 | `webrf_mouv_player` |
| France Bleu Hérault | `fbherault` | 28 | `webrf_mouv_player` |
| France Bleu Isère | `fbisere` | 29 | `webrf_mouv_player` |
| France Bleu La Rochelle | `fblarochelle` | 30 | `webrf_mouv_player` |
| France Bleu Limousin | `fblimousin` | 31 | `webrf_mouv_player` |
| France Bleu Loire Océan | `fbloireocean` | 32 | `webrf_mouv_player` |
| France Bleu Sud Lorraine | `fbsudlorraine` | 33 | `webrf_mouv_player` |
| France Bleu Mayenne | `fbmayenne` | 34 | `webrf_mouv_player` |
| France Bleu Nord | `fbnord` | 36 | `webrf_mouv_player` |
| France Bleu Normandie (Calvados - Orne) | `fbbassenormandie` | 37 | `webrf_mouv_player` |
| France Bleu Normandie (Seine-Maritime - Eure) | `fbhautenormandie` | 38 | `webrf_mouv_player` |
| France Bleu Orléans | `fborleans` | 39 | `webrf_mouv_player` |
| France Bleu Pays d'Auvergne | `fbpaysdauvergne` | 40 | `webrf_mouv_player` |
| France Bleu Pays Basque | `fbpaysbasque` | 41 | `webrf_mouv_player` |
| France Bleu Pays de Savoie | `fbpaysdesavoie` | 42 | `webrf_mouv_player` |
| France Bleu Périgord | `fbperigord` | 43 | `webrf_mouv_player` |
| France Bleu Picardie | `fbpicardie` | 44 | `webrf_mouv_player` |
| France Bleu Provence | `fbprovence` | 45 | `webrf_mouv_player` |
| France Bleu Roussillon | `fbroussillon` | 46 | `webrf_mouv_player` |
| France Bleu Touraine | `fbtouraine` | 47 | `webrf_mouv_player` |
| France Bleu Vaucluse | `fbvaucluse` | 48 | `webrf_mouv_player` |
| France Bleu Azur | `fbazur` | 49 | `webrf_mouv_player` |
| France Bleu Lorraine Nord | `fblorrainenord` | 50 | `webrf_mouv_player` |
| France Bleu Poitou | `fbpoitou` | 54 | `webrf_mouv_player` |
| France Bleu Paris | `fb1071` | 68 | `webrf_mouv_player` |
| France Bleu Elsass | `fbelsass` | 90 | `webrf_mouv_player` |
| France Bleu Maine | `fbmaine` | 91 | `webrf_mouv_player` |
| France Bleu Occitanie | `fbtoulouse` | 92 | `webrf_mouv_player` |
| France Bleu Saint-Étienne Loire | `fbstetienne` | 93 | `webrf_mouv_player` |
<!-- stations:auto:fin -->

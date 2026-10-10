# Extensions

FilmCraft has an **Extensions** menu in the menu bar, next to Window and Help. It lists the
extensions installed for the current user, alphabetically; an extension with several entries gets a
submenu. The last items of the menu install, manage and reload extensions.

An extension is a small folder with an `extension.json` manifest. It drives FilmCraft the way an agent
does: through the same commands the headless CLI (`filmcraft-cli run`) and the MCP server take. There is
no plugin ABI, no native code loading and no `unsafe`: crate `crates/extensions` (`filmcraft-extensions`)
parses the manifest, manages the folder and runs entries; the window and the CLI each pass it a closure
that executes one command. The same extension therefore runs in the window and headless.

> **This is not a host for Adobe CEP / UXP panels or for VST3 / Audio Units / OpenFX plug-ins.** Those
> are separate, much larger projects (see [ROADMAP.md](../ROADMAP.md)). An extension here is a recipe of
> FilmCraft commands, or a program that prints such commands.

## Where extensions live

```text
macOS    ~/Library/Application Support/FilmCraft/extensions/<id>/
Windows  %APPDATA%\FilmCraft\extensions\<id>\
Linux    $XDG_DATA_HOME/filmcraft/extensions/<id>/   (default ~/.local/share/filmcraft/extensions/<id>/)
```

`FILMCRAFT_DATA_DIR` and `--data-dir <dir>` move the data directory, and the extensions with it.
`<id>` must equal the manifest's `id`. Dropping a folder in by hand works: **Extensions ▸ Recharger les
extensions** (or `extensions.reload`) picks it up without restarting.

The extensions in the repository's `extensions-bundled/` folder are offered on first launch of an app
that ships them (next to the executable, or in the macOS bundle's `Resources`):

* the ones that only drive FilmCraft's own commands (all but one of the nine) are installed once; one the user
  removes stays removed (`.bundled-seeded.json` in the extensions folder remembers the offer);
* **an extension that starts an external program (Storyboard Maker) is never installed silently.** A
  dialog, *Extension fournie avec FilmCraft*, names it, shows the program it would run and what that means
  (it runs with your user's permissions), and installs nothing until you press *Installer*. *Plus tard*
  closes the question until the next launch (the status bar says where to find it); *Ne plus proposer*
  remembers the refusal. Either way the extension stays listed under *Fournies avec FilmCraft, pas
  installées* in *Gérer les extensions…* with the same warning and an install button
  (`extensions.dialog.offer.install.<id>`). Commands: `extensions.offers`, `extensions.installBundled {id}`,
  `extensions.declineBundled {id}`; the dialog's ids are `extensions.offer.install.<id>`,
  `extensions.offer.later`, `extensions.offer.never.<id>`.

## Installing and removing

* **Menu:** *Extensions ▸ Installer une extension…* picks a `.zip` / `.fcext` archive; *Gérer les
  extensions…* opens the manager (install from a file or a folder, remove, reload, run an entry, see
  what failed to load and why).
* **CLI:** `filmcraft-cli extensions list | install <folder|archive> | remove <id> | validate <path> |
  pack <folder> <out.fcext> | run <id> [entry] [--var name=value] [--select id,id,...]`. A headless session starts
  with nothing selected: `--select 12,13` selects those clips (`timeline.select`) before the entry runs, which
  the entries that `require` a selection need. `install` on a folder that holds
  several extensions (such as `extensions-bundled/`) installs each one.
* **Commands** (menu, control channel, MCP bridge): `extensions.list`, `extensions.install {path}`
  (`{folder:true}` or no path opens a picker), `extensions.remove {id}`, `extensions.reload`,
  `extensions.manage`, `extensions.run {extension, entry?, vars?, background?}`, `extensions.status`,
  `extensions.cancel`, `extensions.offers`, `extensions.installBundled {id}`, `extensions.declineBundled {id}`.

Installing the same `id` again updates it; the previous version stays in place if the new package is
refused. An archive is a zip with `extension.json` at its root or inside one top-level folder; `.fcext`
is just a `.zip` with another name.

A package is refused, with the reason, when the manifest is invalid, a file it names (`jsonl`, a program)
is missing, it holds a symbolic link, a path leaves the extension (`..`, absolute paths, drive letters),
it has more than 2000 files, a file over 64 MiB or more than 256 MiB in total, or it is not a zip at all.
Nothing is half-installed.

## `extension.json`

```json
{
  "formatVersion": 1,
  "id": "silence-remover",
  "name": "Silence Remover",
  "version": "1.0.0",
  "description": "Cuts the silences out of the active sequence.",
  "author": "Someone",
  "entries": [
    {
      "label": "Couper les silences",
      "description": "Shown as a tooltip in the manager.",
      "requires": ["sequence"],
      "action": { "commands": [
        { "id": "sequence.removeSilence", "params": { "minSeconds": "${var.min_silence|0.5}" } }
      ] }
    }
  ]
}
```

| Field | Rules |
|---|---|
| `formatVersion` | optional, `1` |
| `id` | `[a-z0-9][a-z0-9._-]{0,63}`, no `..`, not ending in `.`: it is the folder name |
| `name`, `version` | name 1 to 80 characters; version such as `1.0.0` |
| `description`, `author` | optional text (500 / 120 characters) |
| `entries` | 1 to 32. One entry shows the extension's `name` as the menu item; several make a submenu named after the extension with each entry's `label` (unique within the extension) |
| `entries[].requires` | any of `project` (saved), `sequence` (one is open), `selection` (clips selected). The menu item is disabled, and running it refused, until they hold |
| `entries[].action` | exactly one of `commands`, `jsonl`, `exec` |

Unknown fields are errors ("unknown field colour"), as are control characters, over-long text and a
manifest over 256 KiB. The error names the field and the entry.

### Action: `commands` and `jsonl`

A list of FilmCraft commands run in order: `{"id": "<command id>", "params": {…}, "continueOnError":
false}`. The ids and parameters are those of `filmcraft-cli commands` / `describe <id>` and
[agents.md](agents.md). `"jsonl": "steps/run.jsonl"` points at a file of the same objects, one per line
(`#` comments and blank lines allowed), as `filmcraft-cli run` reads them. The first failing step stops
the entry and is reported with its number, id and message, unless it has `continueOnError`.

In the window the commands go through the same entry point as menus and shortcuts, so UI commands
(`mode.export`, `window.panel.*`) work too; the CLI only knows engine commands (`filmcraft-cli extensions
validate` lists the ones it does not know). An extension can't start extensions.

### Variables

`${name}` in any string of a step's `params` (and in `exec.args`):

| Variable | Value |
|---|---|
| `${project}` / `${project_dir}` / `${project_name}` | path of the saved project / its folder / its file name without `.fcproj`. Not available in an unsaved project |
| `${selection}` / `${selection_count}` | ids of the selected clips (a JSON array) / how many |
| `${selection_first}` / `${selection_rest}` | the first selected clip (a number) / the others (an array; needs two clips or more): lets an entry use one clip as the source and the rest as targets |
| `${playhead}` / `${playhead_ticks}` | playhead position in seconds / in ticks (254 016 000 000 per second) |
| `${extension_dir}` / `${data_dir}` / `${home}` / `${temp}` | folders |
| `${blocking}` | `true` in the CLI, `false` in the window: pass it as `wait` to commands that otherwise wait (an export waits in the CLI, runs as a background job in the window) |
| `${host}` | `cli` or `app` |
| `${var.name}` | a value given by the caller (`--var name=value`, or `vars` of `extensions.run`) |

A string that is exactly one variable keeps its type (`"${selection}"` is an array, `"${playhead}"` a
number); inside longer text the value is written in (`"${project_dir}/${project_name}.srt"`).
Alternatives: `${var.min|0.5}` takes the first one that has a value; a number or `true`/`false` is a
literal of that type, a quoted string is a text literal (`"${var.language|\"auto\"}"`). `$${` writes a
literal `${`. A variable with no value stops the step with a message such as `${project} is not
available: it needs a saved project`.

### Action: `exec` (a program)

```json
{ "label": "Run my script", "action": { "exec": {
    "program": "scripts/run.sh", "args": ["${project}"], "timeoutSeconds": 300 } } }
```

`program` is a file inside the extension (`scripts/run.sh`: keep its executable bit; archives keep it) or
a bare command name found on `PATH` (`python3`). It runs in the extension folder with
`FILMCRAFT_EXTENSION_DIR` and `FILMCRAFT_EXTENSION_ID` set, and receives **one JSON object on its standard
input**:

```json
{ "formatVersion": 1, "host": "app", "extension": {"id": "…", "version": "…", "dir": "…"},
  "entry": {"index": 0, "label": "…"}, "project": "/…/film.fcproj", "projectDir": "/…", "projectName": "film",
  "selection": [12, 13], "playheadSeconds": 12.5, "playheadTicks": 3175200000000,
  "hasSequence": true, "dataDir": "/…", "vars": {} }
```

Every line it prints that is a JSON object with an `"id"` is a FilmCraft command, run after the program
has exited (`{"id":"markers.add","params":{"name":"x"}}`); `{"log":"text"}` and any other line are shown
to the user. A program can therefore plan edits but not read their results. It is stopped after
`timeoutSeconds` (default 300, at most 3600), its output is read up to 8 MiB and at most 5000 commands
are taken. A failing exit status is reported with the last lines of its standard error.

**An `exec` entry runs code with your user's permissions**, like any script you would run yourself. The
installer and the manager say so; keep only extensions you trust. Nothing is sandboxed.

## Running an entry: feedback and the window

**What it did.** Every command the bundled extensions use returns a `summary`, one line in French, and
the status bar shows the summaries of the steps joined with " · " (the CLI prints the same line on
standard error and keeps the JSON report on standard output; the report's `log` lists each step's line,
the Events panel gets an Info line for the commands that do work). Examples: « 63 marqueurs posés, 120
BPM », « 5 hésitations supprimées (heu ×3, ben, bah), 1,71 s retirées », « 4 silences supprimés, 6,3 s
retirées (seuil −48 dB) ». When there is nothing to do the line says why (« Aucun silence de 0,5 s ou
plus (seuil −48 dB) : rien à supprimer »), and the commands that cannot do anything refuse with a clear
message (« 0 beat détecté dans ce clip : pas de pulsation régulière… », « 0 coupe : les 63 beats… ne
tombent sur aucun des 2 clips sélectionnés »). A command without anything useful to say adds nothing and the
line falls back to « N commands done ».

**Not freezing the window.** Menu and manager runs go to a worker thread: the entry's commands are served
by the window between two frames (one at a time, on the UI thread, so UI commands and file dialogs work as in
a script), a program of an `exec` entry runs on the worker, and a **transcription** (`sequence.transcribe`,
`transcript.generate`) is recognised on its own thread: the window keeps repainting, the status bar shows
« transcription… 37 % » and a small window, *Extension en cours*, shows the progress with an *Annuler*
button (`extensions.running.cancel`; the transcription stops at its next progress report, a program is not
interrupted before it ends or times out, and no further command starts). While an entry runs the sequence is
locked: editing commands from the menu, shortcuts or the control channel are refused with a message (view and
playback commands keep working), because the entry's later steps read the sequence as the
earlier ones left it. One entry at a time. The engine splits the work in three
(`transcript::begin_transcribe` → `TranscribeJob::run` → the command itself, which finds the recognised speech
in `Session::finished_transcription`), so the undo step, the log and the saved project are those of an ordinary
`transcript.generate`. Scripts and automation are unchanged: `extensions.run` (control channel, MCP) runs the
entry synchronously and returns the report, unless `background:true` is passed; the headless CLI is
synchronous by nature. Measured in the packaged app (whisper-base, 17 s French recording, AutoEdit): 44 status requests were answered
during the 14 s transcription (the first in 1.9 s, the others within 0.25 s; before, the window answered none
until the end), an edit sent meanwhile was refused with the message, and the line at the end read « 37 mots
transcrits (langue : fr) · 5 hésitations supprimées (heu ×3, ben, bah), 1,71 s retirées ; « eux » gardé (pas
isolé) · 7 silences supprimés, 3,6 s retirées (seuil −65 dB) ». Whisper reports its progress once per 30 s
window, so a short clip shows 0 % then 100 %; a long recording shows the steps. What still blocks the window: the other long commands run from an entry
(`sequence.removeSilence` renders the whole mix on the UI thread: seconds on an hour of audio; an export
already runs in the background by itself).

## Bundled extensions

`extensions-bundled/` holds ten extensions, in French like their menu entries:

| Extension | Entries | Built on |
|---|---|---|
| **Silence Remover** | Couper les silences · (serré) · (doux) | `sequence.removeSilence`: the sequence mix is rendered through the mixer graph, measured in 10 ms RMS windows, the quiet/loud threshold comes from the recording's noise floor and speech level (the one speech tightening uses, `filmcraft_speech::vad::threshold`), and the quiet stretches of at least `minSeconds` are ripple-deleted on every unlocked track, keeping `keepSeconds` of air next to speech, as one undo step. No transcript, no model. `apply:false` only reports the ranges. |
| **Sous-titres** | Générer les sous-titres · Générer et exporter en .srt · Exporter les sous-titres en .srt | `sequence.transcribe` (built-in Whisper, CPU), `transcript.createCaptions`, `captions.export`. Needs a build with the `whisper` feature and the speech model installed; otherwise the first step fails with "speech-to-text is not available in this build". |
| **Export en un clic** | Export rapide (dernier preset) · YouTube 1080p · YouTube 4K · Vimeo 1080p | `export.quick` with a built-in preset, written next to the saved project (`"path": "${project_dir}/"`, named after the sequence). |
| **Beat Edit** | Marquer les beats · Marquer toutes les 4 beats · Couper sur les beats · Couper toutes les 4 beats | `sequence.markBeats` / `sequence.cutOnBeats` (below). |
| **Copy Pasta** | Copier les attributs · Coller les attributs · Coller seulement les effets · Coller seulement mouvement et opacité · Premier clip vers les autres (tout / effets seulement) | `edit.copy` + `edit.pasteAttributes` (motion, opacity, volume, panner, time remapping, standard effects with their keyframes). The one-step entries select the first selected clip, copy it, paste on the others (`${selection_rest}`) and restore the selection. The Copier / Coller pair needs the clipboard of a session that stays open, i.e. the window. |
| **AutoEdit** | Montage auto (français) · (français, « eux » toujours coupé) · (anglais) · Retirer les hésitations | `sequence.transcribe`, `transcript.removeFillers`, `sequence.removeSilence`: a first cut of talking footage. Each stage is its own undo step and says what it did. French list (`language:"fr"`): euh, euhh, euhhh, heu, heuh, hum, humm, hmm, hmmm, mmh, mm, mmm, hem, ben, bah, beuh; plus **« eux » only when it is a hesitation standing alone** (see below). Whisper often leaves hesitations out or mis-spells them, so only what it wrote can be cut by word; the silences are cut from the audio level whatever the transcript says. |
| **Nettoyage de la voix** | Nettoyer la voix (léger) · (standard) · (fort) · Installer le modèle de voix (DeepFilterNet3) | `audio.denoiseVoice`, `models.install` (§ Neural tools below). Needs the 8 MB model, downloaded after you accept its licence. |
| **Reframe vertical** | Recadrer en 9:16 · 9:16 (sous-titres) · 1:1 · 4:5 · Installer le détecteur de personnes (YOLOX-Nano) · Installer le détecteur de visages (YuNet, facultatif) | `sequence.autoReframe`, `models.install` (§ Neural tools below). Needs YOLOX-Nano (3.7 MB); YuNet (0.2 MB) is optional. |
| **Storyboard Maker** | Exporter une image par plan · Exporter une image par marqueur · Assembler la planche (PNG + PDF) | `exec` entries running `scripts/storyboard.sh` (a Python 3 program): it reads the saved `.fcproj`, and prints `playhead.set` + `file.exportFrame` for each shot (the clips of the first video track that has clips) or marker. The stills (`<project>-storyboard/001.png…` with an `index.json`) come from the Program monitor. The sheet entry lays them out with titles and timecodes (`<project>-storyboard.png` and `.pdf`, `--var columns=4`) and needs the Pillow package. The program only plans: the sheet is built by a second entry because a program cannot read the results of commands that run after it exits. Save the project first. It is the only bundled extension that starts a program, so it is offered on first launch (a dialog) and never installed silently. |
| **Sous-titres animés** | Karaoké (3 mots) · Pop, un mot à la fois · (transcription existante) × 2 | `transcript.createWordCaptions` (below): graphic clips with keyframes from the word timestamps. Variables `accent`, `color`, `words`, `y`, `font`, `language`. |
| **Traduction des sous-titres** | FR → EN · EN → FR · Transcrire, sous-titrer et traduire (EN / FR) · Exporter la piste traduite en .srt | `captions.translate` (below): offline Opus-MT, second caption track with the same timings. |
| **Tics de langage** | Retirer les euh (turbo, français) · uh/um (turbo, anglais) · (base, français) · (transcription existante) | `sequence.transcribe {hesitations:true}` with Whisper large-v3-turbo or base, then `transcript.removeFillers`, then `sequence.removeSilence`. |

### Beat editing: `sequence.markBeats` and `sequence.cutOnBeats`

The engine analyses an audio clip with the beat grid of `filmcraft_audio_dsp::beatgrid` and works on its
beats. Both commands are in the registry (CLI `exec`, the control channel, MCP) and have no menu item: the
menu is Extensions ▸ Beat Edit.

| Command | Parameters | Result |
|---|---|---|
| `sequence.markBeats` | `clip` (audio clip; default the selected one, else the first of the sequence), `every` = 1, `offset` = 0, `subdivide` = 1, `bpm` (0 = automatic; the tempo octave to use, 30-300), `downbeat` = false, `name` = "Beat", `color`, `snap` = true, `apply` = true | `{clip, bpm, beats, detected, markers, times, barStart, barConfidence, summary}`. Markers "Beat N" on one beat in `every` (N counts the kept beats: bar numbers for `every:4`); one undo step; a marker already there is not doubled. `apply:false` only reports |
| `sequence.cutOnBeats` | `beatClip`, `clips` (default: the selection without the beat clip, or the beat clip itself when alone), `every`, `offset`, `subdivide`, `bpm`, `downbeat`, `snap`, `apply` | `{clip, bpm, beats, detected, cuts, times, barStart, barConfidence, summary}`. Razor (`filmcraft_edit::razor_items`) of those clips and their linked partners at each beat; one undo step |

**Tempo octave.** The beat tracker of Clip ▸ Remix takes the tempo of the strongest autocorrelation peak; on
real drums that peak is often the wrong octave. `beatgrid` keeps the same onset envelope and tracker but
chooses differently: the few strongest peaks and their halves and doubles inside 65-172 BPM are candidates; for
each, a rigid comb of beats is fitted (period within ±3 %, every phase) and scored by precision (share of teeth that
meet an onset) and recall (share of the onset strength the teeth explain) combined as an F-score, plus a weak
preference for the usual range. A half tempo explains too few onsets (the snare falls between its beats), a double tempo
on hi-hats has teeth with no onset, so the real pulse wins. The winner is then tracked with a stiff
dynamic-programming tracker (a click or a pre-hit a few frames before the real attack no longer pulls the path),
a beat that still sits more than 15 ms off the local line of its neighbours is put back on it, a clearly steeper
attack 25 to 60 ms away (measured on the signal, in dB per millisecond) replaces the envelope's peak, and weak or
off-line beats at the edges are dropped. A clip where the winner explains less than 60 % of its teeth or 30 % of the
onset strength (a pad, speech, noise) is refused with « 0 beat détecté dans ce clip… ». `bpm` selects the
octave (the nearest tempo in the audio is used; with a hint the recall gate is off, you said there is a pulse);
`every`/`subdivide` thin out or add beats.

**Bars.** `downbeat:true` looks for the first beat of the bars among the four phases, scoring each beat on its
onset accent, the low-frequency energy in the 80 ms after it (the kick) and the chroma change against the beat
before; the phase with the best mean wins and `barConfidence` (0-1) is the gap to the runner-up in standard
errors. With chord changes on the bar lines the answer is right and confident (generated music, 5 tempi:
5 of 5, confidence 0.73-1.0); on bare drums beats 1 and 3 (kick) look alike and half the answers are half a bar
off, with a confidence of a few per cent, which the status line reports (« mesure dès le beat 2 (incertain :
13 %, à vérifier) »).

**Measured** (CLI, `Marquer les beats` and `Marquer toutes les 4 beats`, generated drums: kick on 1 and 3,
noise snare on 2 and 4, 16 bars, 48 kHz, 24 fps project; error = distance of each frame-snapped marker to the
nearest true beat):

| | v1 (previous build) | now |
|---|---|---|
| 90 BPM with hi-hats | 180.57 BPM, 127 markers (off-beats), max error 333 ms | 90.00 BPM, 63/63, 0.0 ms |
| 100 / 120 / 128 / 140 BPM with hi-hats | right octave, 25.0 / 0.0 / 20.8 / 23.8 ms | right octave, 25.0 / 0.0 / 20.8 / 23.8 ms |
| 90 / 100 BPM without hi-hats | right octave, 0.0 / 25.0 ms | right octave, 0.0 / 25.0 ms |
| 120 / 128 / 140 BPM without hi-hats | half tempo (59.9 / 64.0 / 70.1 BPM), 32 of 63 markers | right tempo, 63/63, 0.0 / 20.8 / 23.8 ms |
| worst error over the 10 cases | 333 ms (4 of the 10 cases at the wrong octave) | 25.0 ms (< 1 frame = 41.7 ms; the floor of any result is the frame snap, up to 20.8 ms) |

The unit tests (`cargo test -p filmcraft-audio-dsp beatgrid`) sweep 84 more drum cases (two sample rates, three
snare levels, 90-150 BPM, with and without hats): right octave and worst beat error 14 ms before frame snapping;
generated music with bass and chord pads at 90-140 BPM: right tempo, worst error 10 ms; silence, noise and a slow
pad: refused. Limits that remain: **one steady tempo** (65-172 BPM without a hint; a 175 BPM drum-and-bass
track is read at 87 unless `bpm` says otherwise); a slow tune (say 80 BPM) with hi-hats on the eighths can still
be read at double (160) because the eighths explain everything: give `bpm` or use `every:2`; the bar start on bare
drums is a coin toss between beats 1 and 3 (the status line says so); **a beat on the very first frames of
the clip is not reported** (the onset envelope has no previous frame to differ from); clips with a speed change,
reverse or a Remix are refused.

### Hesitations: `transcript.removeFillers` in French

Whisper-base on a French voice writes a spoken « euh » as `Heu ?`, `Heu.`, `eux`, `eux,`, drops others and
turns « bah » into « bas ». The command therefore takes a configurable list and one contextual rule:

* `fillers` replaces the list, `language` picks the default (`fr`: the list above, `en`: um/uh/erm/…,
  `auto`: the transcript's language), `also` adds words, `conditional` names words that are cut **only when
  isolated** (default in French without an explicit `fillers`: `eux`).
* A conditional word is isolated when, **on the audio** (the mix in 10 ms RMS windows, quiet/loud threshold from the
  recording's noise floor), the sound around its loudest moment lasts less than `isolatedMaxSeconds` (0.4) and
  has `isolatedPauseSeconds` (0.15) of quiet before and after. Word times are not used for this: a
  transcriber stretches a word over the silence next to it (a 0.9 s « Ben », a « tous. » that lasts 1.2 s
  and swallows the euh), so a « pour eux » looks as isolated as a « euh » by timestamps. The result lists
  what was `kept` and why (« le son dure 1,30 s (limite 0,40 s) : le mot est dans la phrase »).

Measured on two `say -v Thomas` recordings already transcribed by whisper-base (`hes_isolated`: Euh, Heu,
Hum, Ben, Bah each between pauses and a real « pour eux »; `hes_inline`: six hesitations glued into sentences
and a real « pour eux »):

| | previous list (v1) | « eux » always cut (v1 entry 2) | now (FR list + isolated « eux ») |
|---|---|---|---|
| isolated: hesitations cut / spoken | 3 / 5 | 3 / 5 | **5 / 5** |
| isolated: real words cut | 0 | 1 (« pour eux ») | **0** |
| inline: hesitations cut / spoken | 0 / 6 | 3 / 6 | **2 / 6** |
| inline: real words cut | 0 | 1 | **0** |

On the original test voice (`voice_fr`, 28 words, two « eux, »): the previous default cut 0, the « eux » entry cut
both, the new default cuts the one that stands alone (0.21 s) and keeps the other (a 1.23 s sound inside a
phrase); no script was recorded for that voice, so only counts are given.

What stays impossible here: a hesitation **glued to its neighbours without a pause** (`puis heu on…`, `donc hum
voilà`) is a sound inside a phrase, for the audio as for the text, and is left alone (the « eux » entry that
cuts every `eux` gets 3 of the 6 inline ones at the price of cutting a real « pour eux »); a hesitation that
whisper **did not write at all** (the first euh of the inline sample was swallowed by the neighbouring words'
times) or wrote as another word (« bah » → « bas ») cannot be found from the text, and no acoustic detector
of filled pauses is attempted (it would need a model of its own); and « ben » / « bah » are cut everywhere in French
(`ben oui` loses its `ben`): remove them from the list with `fillers` if that is wrong for your speaker.

### Whisper large-v3-turbo and keeping the « euh »

* **Model.** `whisper-large-v3-turbo` (OpenAI, MIT; 809 M parameters, 32-layer encoder, 4-layer decoder, 128 mel bins, 1.6 GB of safetensors pinned to revision `41f01f3f`, SHA-256 checked, downloaded after the usual dialog) is in the catalogue and in Settings ▸ Media Analysis. The existing candle Whisper implementation already supports its architecture (the config gives the layer counts, the 128 mel bins and the alignment heads; no code change was needed for the network). It needs about 3 GB of memory (the weights are converted to f32) and loads in 15-50 s the first time; transcription ran at about 2× real time on this Mac's CPU (48 s of French in 23 s). `whisper-large-v3` itself (3 GB, 32 decoder layers) was not added: the decoder is eight times heavier for little gain here.
* **Why « euh » is missing.** Whisper was trained on cleaned transcripts and writes « eux », « homme » or nothing for a spoken « euh »; the large models clean more than the small ones (turbo alone found 10 of 27 hesitations below, base 15).
* **What is in the engine.** `sequence.transcribe` / `transcript.generate` take `hesitations: true`, `prompt: "…"` and `hesitationBias: x`. The *initial prompt* is the text Whisper reads as what was said just before the audio: `hesitations:true` uses « Euh, alors, heu, je pense que, hum, ben, c'est, euh, bah, voilà. » in French (an English one for `en`; `prompt` overrides it with a text in your own speaker's style). The prompt needs a tokenizer that *encodes*: `Tokenizer::encode` (GPT-2 byte-level BPE, written for this) was added next to the decoder. `hesitationBias` adds a logit bias (2 when `hesitations` is on) to the hesitation spellings that are a single token (um, uh, hum, hmm, ben, bah… — « euh » and « heu » are several tokens in the multilingual vocabulary, so for them the prompt does the work). A detector of filled pauses on the audio was tried and abandoned: the `say` voice's « euh » lasts 0.22 s and « heu » 0.13 s, which is an ordinary syllable, so no acoustic rule tells them apart.
* **Measured** (CLI, French, `say -v Thomas`; 3 recordings, 27 spoken hesitations with exact times for the 48 s synthetic one, 3 real « eux » (« pour eux ») among them; `transcript.removeFillers` with the French list and the isolated « eux » rule; a hesitation counts when a cut falls within 0.25 s of it):

| | hesitations cut | other cuts |
|---|---|---|
| 0.4.0-ext (whisper-base, default) | 15 / 27 | 0 |
| base + `hesitations` | 20 / 27 | 2 (outside the 0.25 s window of any spoken hesitation; no real word cut) |
| turbo, plain | 10 / 27 | 0 |
| **turbo + `hesitations`** | **27 / 27** | **0** (the 3 real « eux » kept) |

  Prompt-only and bias values were swept with the `transcribe` example, whose scorer counts every « eux » as a cut (so its totals are not the table's: turbo: prompt 26 / 27 with one extra cut, + bias 2 27 / 27, + bias 4 27 / 27 with two extra cuts; base: prompt 24 / 27, bias alone worse). **Limits:** one synthetic voice, so the figures say the mechanism works, not how often it holds on real speech (a prompt can also make Whisper invent a « euh »; not measured on human voices). A transcription with the prompt costs the same time. The Extension *Tics de langage* wires it up.

### Animated word captions: `transcript.createWordCaptions`

The words of the sequence transcript become graphic clips on a new video track « Sous-titres animés » (one undo step, ordinary editable clips: text layers with keyframes).

| Parameter | |
|---|---|
| `style` | `karaoke` (the group stays on screen, the word being spoken takes the accent colour) or `pop` (one word at a time, scale 70 → 112 → 100 %) |
| `wordsPerGroup` | 1-6 (default 3; pop 1); groups also end at a sentence end, a pause (`pauseSeconds`, 0.6), a speaker change or `maxChars` (18) |
| `color` `accent` `outline` | `#rrggbb` (white, #ffd400, black) |
| `font` `fontStyle` `size` `y` `caps` | Inter Bold, ~9.5 % of the frame height, baseline at 74 % of the height, capitals |
| `holdSeconds` | how long a group stays after its last word (0.12), never into the next clip |

How: the clip of a group lasts from its first word to its last. For karaoke a second text layer with the same text sits over the first; all its characters but the active word's have a transparent fill (per-character runs), and its fill alpha is keyframed with *hold* interpolation, so the two layers lay out identically without measuring the text. Every layer pops in with scale and opacity keyframes. Rendered through the normal graphics pipeline (checked on frames of a 1280×720 test project: the colour follows « Alors, on commence par le début » word by word). Limits: the outline is a property of a whole layer, so a progressive « words appear one by one » style was left out; a sentence longer than `maxChars` per word group is split, not wrapped; the font must exist on the machine (Inter is the engine default).

### Offline translation: `captions.translate`

`captions.translate {track?, to:"en"|"fr", from?, name?, beams=4, maxChars=42}` copies a caption track into a new one (right after it, language tag set, same timings and speakers) with every caption translated; a two-line caption is translated as one sentence and wrapped again in two balanced lines. `from` defaults to the track's language, which `transcript.createCaptions` now sets from the transcript.

* **Models.** Opus-MT *tc-big* (Helsinki-NLP, CC-BY-4.0: credit the source), one per direction, 230 M parameters, 464 MB each, pinned to a revision and SHA-256 checked: `opus-mt-tc-big-fr-en`, `opus-mt-tc-big-en-fr`. They are listed with size, source and licence under `translation` in `transcript.models` and downloaded with `transcript.downloadModel {model}`; the command refuses, saying so, when the model is not there. The smaller `opus-mt-fr-en` has no safetensors on the Hub and `opus-mt-en-fr` neither; NLLB-200 (CC-BY-NC) and M2M100 need other architectures and were not attempted.
* **Implementation.** Pure Rust on candle, no new dependency: `crates/speech/src/mt/`: `marian.rs` (the Marian network written from the architecture: post-LayerNorm transformer, scaled embeddings, sinusoidal positions, tied output with `final_logits_bias`, beam search with a key/value cache per beam), `spm.rs` (a SentencePiece *unigram* reader — the `.spm` protobuf — with Viterbi segmentation; the `nmt_nfkc` normaliser is approximated by the few substitutions that matter in subtitles, so rare compatibility characters can come out as `<unk>`). Behind the `translate` feature (enabled by the engine's `whisper` feature, so the app builds that have speech have translation).
* **Measured** (this Mac, CPU, 4 beams): the model loads in 13 s; « Bonjour à tous, aujourd'hui on va parler de montage vidéo. » → « Hello everyone, today we are going to talk about video editing. » in 1.0 s; « Je ne sais pas trop, mais c'est une bonne idée de commencer par là. » → « I don't know, but it's a good idea to start there. » in 1.2 s; English → French « Hello everyone, today we are going to talk about video editing. » → « Bonjour à tous, Aujourd'hui, nous allons parler du montage vidéo. ». On a transcribed 4-caption track: 4 s for the four, the SRT of the second track written with `captions.export {track:"C2"}`. No BLEU score was computed (no reference set); the quality is whatever tc-big gives on a few sentences, and the English output keeps « Uh » when the transcript has « Heu » (remove the hesitations first). The command runs on the calling thread: about a second per caption, the window waits.

## Neural tools: voice clean-up and auto-reframe

Two commands run small neural networks on the CPU, in pure Rust: no ONNX runtime, no Python, no network at
run time. The weights are **optional downloads**, never bundled and never in the repository.

| Crate | Layer | Role |
|---|---|---|
| `filmcraft-nn` | L1 | an ONNX interpreter of our own (the file is parsed here; 30 operators: 2-D `Conv` / `ConvTranspose`, `GRU`, two-operand `Einsum`, `MatMul`, `MaxPool`, nearest `Resize`, shape operators…). No dependency. Convolutions use `im2col` and a blocked product on scoped threads. Checked against `onnxruntime` on the seven real models below: largest relative difference 2.4 × 10⁻⁵ (YOLOX-S), 1 × 10⁻⁶ for the speech model; 4 to 6 times slower than onnxruntime, which is still 30× faster than real time for the voice. Never panics on a hostile file (truncations and byte flips are tested) |
| `filmcraft-modelstore` | L2 | the catalogue and the folder `<data dir>/models/<id>/`: URL, SHA-256, size and licence of each file; download (feature `download`, pure-Rust TLS) into `<name>.part`, SHA-256 check, rename; the `.tar.gz` pack of DeepFilterNet3 is verified, then only its three named members are unpacked, each verified again |
| `filmcraft-voice` | L2 | DeepFilterNet3: STFT (960 points, Vorbis window, 480 hop), 32 ERB bands, exponential mean normalisation, the three graphs, deep filtering, overlap-add |
| `filmcraft-reframe` | L2 | YOLOX and YuNet decoding, subject choice, crop path |

### The models

| id | for | download | licence |
|---|---|---|---|
| `deepfilternet3` | voice clean-up | 8.0 MB pack from `Rikorose/DeepFilterNet` tag v0.5.6; 3 ONNX graphs, 1 million weights | MIT OR Apache-2.0 |
| `yolox-nano` (default) · `yolox-tiny` · `yolox-s` | people | 3.7 · 20 · 36 MB from `Megvii-BaseDetection/YOLOX` release 0.1.1rc0 | Apache-2.0 |
| `yunet` | faces (optional) | 0.2 MB from `opencv/opencv_zoo` (2023 March) | MIT |

* `models.list` shows each model's size, source, licence, files with SHA-256, and whether it is installed.
* `models.install {"model": "…", "accept": true}` downloads one. **Without `accept: true` nothing is
  downloaded**: from the CLI or the control channel the command refuses and says what it would fetch; in the
  window the same call opens the licence dialog (*Modèle optionnel*: task, size, source link, licence link, and
  *Accepter la licence et télécharger* / *Annuler*; ids `models.consent.accept` and `models.consent.cancel`), and
  the download runs on its own thread with a progress bar. A file whose SHA-256 differs is deleted.
* Needs a build with the `models` feature (the packaged app and CLI have it).
* **Licences.** Licence is not a criterion for personal use, but no GPL, LGPL or AGPL code was read or copied:
  the voice pipeline is written from the algorithm of libDF (MIT OR Apache-2.0), YOLOX and YuNet are decoded
  from their own papers' conventions. Training-data licences were not re-read: the licence shown is that of the
  code and the weights.

### Voice clean-up: `audio.denoiseVoice`

| Parameter | Meaning |
|---|---|
| `clips` | audio clips (default: the selected ones) |
| `strength` | `light` (noise cut by at most 6 dB), `standard` (14 dB, default), `strong` (all the model finds) |
| `amount` | 0-100, overrides `strength`: the original is mixed back into the spectrum, `1 − amount` of it, so the noise reduction is capped at `20 log₁₀(1 − amount)` dB |
| `dir` | where the cleaned WAV is written (default: Scratch Disks ▸ Captured Audio, else the project folder, else the data folder) |

What it does: reads the audio the clip plays (any rate, up to 15 min per clip; at most two channels, a duplicated
channel is processed once), runs the model, writes `<name> (voix nettoyée).wav` (32-bit float), imports it and
points the clip at it. Place, length, effects, keyframes, gain and links are untouched; the original stays in
the project; **one undo step** restores the clip (the WAV stays on disk). Clips at another speed, reversed or
remixed are refused. The result carries, per clip, the **noise floor** (10th percentile of the 20 ms levels),
the **speech level** (90th percentile) and their difference, before and after: « bruit de fond −27,7 → −120,0
dBFS ». The model zeroes the frames it takes for noise alone (libDF's own gate: local SNR below −10 dB), so with
`strong` the pauses become digital silence (floor −120); `light` and `standard` keep a trace of the room.

Runs on the calling thread (synchronous in the window, like the other long commands of an extension).

**Measured** (a 31 s French recording made with `say` voices Thomas and Amélie, three pauses of 1.5 s, plus
noise added at a stated SNR measured over the speech; reference = the clean recording; `speech SNR` = SNR of
the output against the clean recording over the frames where the voice is; `pause floor` = level of the
frames where it is silent; one run, Apple-silicon CPU):

| noise | noisy: speech SNR · pause floor | cleaned (`strong`): speech SNR · pause floor | time for 31 s |
|---|---|---|---|
| white, 5 dB | 4.2 dB · −27.7 dBFS | **16.1 dB** · −81 dBFS | 1.1 s |
| white, 15 dB | 14.2 dB · −37.7 dBFS | **22.0 dB** · −88 dBFS | 1.2 s |
| ventilation (low-pass noise), 10 dB | 9.2 dB · −32.7 dBFS | **15.7 dB** · −78 dBFS | 1.1 s |
| 50 Hz hum + harmonics, 10 dB | 9.2 dB · −32.7 dBFS | **18.2 dB** · −56 dBFS | 1.2 s |
| kitchen (hiss + 60 clatters), 5 dB | 4.2 dB · −27.7 dBFS | **16.1 dB** · −87 dBFS | 1.2 s |

Gain on the voice 6.5 to 12 dB; the clean recording through the same pipeline comes back at 29.8 dB SNR (log-
spectral distance 1.2 dB): the voice is preserved. `strength` caps work as announced (white 5 dB: pause floor
−41.6 dBFS at 80 %, −33.7 dBFS at 50 %, i.e. 14 and 6 dB below the noisy −27.7). Limits: synthetic speech and
synthetic noises, no real room recording and no human listening test (SNR and spectral distance say nothing
about artefacts a listener would hear: on the strongest setting, hum harmonics and the pause floor are the
places to listen); DeepFilterNet is trained on 48 kHz speech, music or singing are not what it is for; the
two-stage deep filter runs on every frame the gate lets through (libDF skips it above 20 dB local SNR; here
that skip dropped a 14 dB recording to 8.7 dB, see `crates/voice`); no real-time preview.

### Auto-reframe: `sequence.autoReframe`

Re-frames the selected video clips on their main subject so a 16:9 shot fills a 9:16, 1:1 or 4:5 frame.
It samples the clip (5 images a second), finds people with YOLOX and, when YuNet is installed, faces,
follows one subject, plans a smooth crop, and writes it as **`Motion` Scale + Position keyframes** (Scale to
Frame Size is turned off): one undo step, everything stays editable in Effect Controls.

| Parameter | Meaning |
|---|---|
| `clips` | video clips (default: the selected ones) |
| `ratio` | `9:16` (default), `1:1`, `4:5`, any `w:h` or `1080x1920` |
| `model` | `yolox-nano` (default, 416 px) · `yolox-tiny` · `yolox-s` (640 px, 4× slower) |
| `faces` / `subject` | use YuNet when installed (default true: the framing centres on the face of the chosen person) · `subject:"face"` keeps only people whose face is found |
| `fps` | detections per second, 0.5-15 (default 5) |
| `smooth` | seconds of smoothing (default 0.3; 0 follows every sample) |
| `deadZone` | the camera holds still while the subject stays within this share of the crop width around it (default 0.03) |
| `safeBottom` | share of the output height kept free under the subject for subtitles (0-0.6); the subject sits in the free part when `zoom` > 1; the result reports `captionOverlapPercent` |
| `zoom` | 1-4 (default 1: the crop is the largest window of that ratio; > 1 zooms in and lets the subject move vertically too) |
| `resizeSequence` | the sequence becomes 1080 wide (or high) at the target ratio in the same undo step. **Refused with a clear message when other video clips would be re-framed by it** (duplicate the sequence, or create one at the right ratio, or pass `true`) |
| `apply` | `false` only plans and reports |

How it works: for a picture `W × H` and a target ratio the crop window is the largest rectangle of that
ratio inside the picture (divided by `zoom`); `Scale` = output width / crop width and `Position` moves the
window's centre to the middle of the frame, so the window never leaves the picture. Each sample picks the
subject (largest × most confident person at first; afterwards the one overlapping or nearest the previous
subject, dropped only for one that ranks twice as high); the samples go through a **dead zone** (the tripod
stays still while the subject fidgets) and an exact **smoothing spline** (acceleration penalty `λ = (smooth·fps)⁴`,
zero phase: no lag on a steady walk). A **scene cut** (the picture changes between two samples: 24 × 14 luma
thumbnails differ by more than 0.2) splits the path into pieces planned separately and the frame cuts with the
picture (Hold keyframe). Keyframes are Linear, one per frame at most, minus those a straight line reproduces
within 0.15 source pixel: Bézier interpolation in this engine eases to a stop at every keyframe (measured:
mean jerk 40 000 px/s³ with Auto Bézier keyframes, 3 000 with Linear ones), so it would stutter a pan. The
result per clip: `samples`, `detected`, `coveragePercent`, `faces`, `cuts`, `keyframes`, `jerkPxPerS3`
(mean and max of the *evaluated* keyframes at the sequence frame rate, in output pixels, and the same for the
unsmoothed detections), `maxSpeedPxPerS`, `maxDistanceToSamplesPx`, `captionOverlapPercent`.

**Measured.** *Ground truth*: a real rider cut out of a motoball frame (YOLOX box, 280 × 445 px, scaled to
640 px high) moved over a real hockey-rink frame, 1920 × 1080, 25 fps, 14 s (stand, walk right at 374 px/s
in output pixels, pause, walk back); the centre of the pasted picture is the true position. 9:16, YOLOX-Nano,
defaults. The re-framed sequence was rendered by the CLI (`file.exportFrame`, 1080 × 1920) every 0.5 s
(27 frames) and the person detected again **in the rendered output**:

| measure | result |
|---|---|
| subject found | 70 of 70 samples (100 %), 3.9 s for the 70 images including decoding |
| centering error in the output (frame 1080 px wide) | mean 19.7 px (1.8 %), rms 24.2 px, max 62.5 px (5.8 %, when the subject stops dead) |
| keyframes | 89 for 14 s |
| speed of the pan | max 392 px/s (the subject's own: 374 px/s) |
| jerk of the played path, mean (max) | **3 044 (15 315) px/s³**, against 25 612 for the unsmoothed detections (8× smoother) and 3 640 for the true path itself (its three direction changes are impulses) |

The same case with the earlier defaults (`smooth` 0.45, `deadZone` 0.06) gave a mean error of 43.7 px and a
max of 122 px: the dead zone trades centering for stillness (the subject rides on the edge of it while the
camera trails, up to `deadZone × crop width`), `0` centres best and moves most. *Real footage*, no ground
truth, only what the command reports: motoball crowd (13 s, 960 × 540, 24 fps, 9 people in view): subject on
74.6 % of the samples, no false cut, jerk 6 000 against 100 500 unsmoothed (17×); a rider close-up:
92 % and 10 400 against 174 000; an empty hockey rink: 0 % subject, the window stays centred (2 keyframes) and
nothing breaks. Detection quality checked by eye on a crowd frame (9 people, 9 boxes).

Limits, honestly: **one subject** per clip and no re-identification: in a crowd the framing can hop from one
person to another (the real crowd clip reaches 2 700 px/s during hops); the vertical position only moves with
`zoom` > 1; faces: YuNet found some in the crowd but its framing was not verified against a reference; no
human viewing test of the result; sampling at 5 images/s misses very fast subjects; the command runs on the UI
thread (14 s of 1080p footage took 3.9 s; 13 s of 960 × 540 with faces took 22 s on a busy machine); only forward-played clips, subjects as
detected by COCO "person"; the new aspect needs the sequence at that ratio (see `resizeSequence`).

### Neural tools: not done (next steps from the backlog, `infra-artcraft.md` § 7)

Left for later, each is a candidate on the same `nn` interpreter or a program: Demucs voice/music separation
(idea 19; needs a `ConvTranspose1d`/LSTM set of operators), L1-optimal stabilisation (idea 20), RIFE
frame interpolation (idea 16), video matting with BiRefNet / SAM 2.1 (idea 18), Whisper-turbo and filler
detection (idea 1), beat tracking with Beat This! (idea 17), and a background thread for these long commands
(they run on the UI thread today). Not started.

## Optional neural tools (build feature `ml`)

Seven extensions (Ralenti RIFE, Agrandir x2, Voix et musique, Détourage de personne, Recherche de rushes,
Stabilisation L1, Modèles IA optionnels) and the « Beat This » entries of Beat Edit drive commands of the engine that run
neural networks in **pure Rust on candle** (Metal on macOS, else the CPU; `FILMCRAFT_ML_DEVICE=cpu` forces the CPU).
Without `--features ml` the commands exist and answer « this build has no neural tools ». **No model is part
of FilmCraft**: each is downloaded on request from its publisher, **after its licence was shown and accepted**,
checked against a pinned size and SHA-256 (a file that fails is deleted), and kept in `FILMCRAFT_ML_MODELS_DIR` or
`<data folder>/models/ml/<id>/` with a `LICENSE.txt`. In the window, an entry whose model is missing opens a
dialog with the licence (`ml.licence.accept.<id>` / `ml.licence.cancel`); in a script, `ml.install {model,
acceptLicense:true}` (without `acceptLicense` it only says what the licence is). Models are used offline.
`ml.models` lists them. `crates/ml` is MIT OR Apache-2.0; `mlfetch`, `mlrestore`, `mlsearch` come from LightCraft
(same authors, Apache-2.0). No GPL / AGPL / LGPL code is in the repository.

| Model id | Size | Licence of the weights | Used by |
|---|---|---|---|
| `rife-v4` | 12 MB | MIT (Megvii, ECCV2022-RIFE; safetensors re-host) | `clip.interpolate` |
| `span-2x` | 4.5 MB | CC BY 4.0 (Phips) / network Apache-2.0 | `clip.upscale` |
| `beat-this-final0` | 81 MB | MIT (CPJKU/beat_this); the authors warn that part of the training data is under limited licences | `sequence.markBeats` / `cutOnBeats` with `model:"beat-this"` |
| `htdemucs` | 84 MB | MIT (Meta, official `955717e8` checkpoint) | `audio.separate` |
| `siglip2-base-224` | 1.5 GB | Apache-2.0 (Google); the tokenizer is Gemma's: check its terms before redistributing | `media.rushIndex` / `media.rushSearch` |
| `rvm-mobilenetv3` | 15 MB | **GPL-3.0: personal use only** (the dialog says so); never redistributed with FilmCraft | `clip.matte` |

Video and audio tools never touch the original: they write a **new file** (ProRes 422 LT by default, `preset`
`prores_hq` / `h264`; audio: 32-bit float WAV) in `<project folder>/ML/` (or `out`) and import it. Parameters
shared by the video tools: `clip` (the used range of a timeline clip, default the selected one) or `item` (a media
item), `out`, `preset`, `wait` (extensions pass `${blocking}`; otherwise a background job, `job.list`). A frame
worker thread of its own makes the frames (the export asks for them from several threads, and a network that needs
them in order or one at a time on the GPU must not be waited on from a work-stealing pool: measured, it hung the
export and, without the guard, corrupted SPAN tiles).

| Command | What it does | Measured (Apple silicon, Metal, 24-30 fps test clips, 720p unless said) |
|---|---|---|
| `clip.interpolate {fps, slow, depth, scale}` | RIFE v4 midpoint network on a binary tree of midpoints (rounded to 1/2^depth of a frame; `depth` 3 = 1/8). `fps:60` converts the frame rate, `slow:4` is a four times slower clip at the same rate (audio dropped). `scale` 0.5 estimates the flow on half the pixels per side. | Candle port equals PyTorch to **95.5 dB** (one level of 255 at worst). Real 30 -> 60 fps test, 9 interpolated frames of a dance clip against the real in-between frames: **45.50 dB** (frame blend 41.02, repeating the previous frame 38.00). Speed: 3.5 s per interpolated frame at scale 1 (482 s for 240 frames), 1.2 s at scale 0.5, **the default above 640x360**: 240 frames in **117 s** instead of 482 s for **45.42 dB** instead of 45.50. Candle's Metal convolution is about 230 GFLOPS here, far from the GPU's peak: real-time is out of reach. |
| `clip.upscale {keepTone}` | SPAN 2x on every frame (tile 1024). `keepTone` (default) puts each channel's mean back: the network darkens the picture (-5.7 grey levels measured). | Clean test (a 1080p clip shrunk with Lanczos, enlarged back, compared with the real 1080p frames): **44.61 dB** for SPAN, **45.72 dB** for Lanczos, so **no measurable fidelity gain on this undegraded input** (SPAN sharpens; PSNR does not reward it) and no flicker (the error changes by 1.64 grey levels between frames against 1.53 for Lanczos). Without `keepTone` the same run gave 32 dB. 4.2 s per 960x540 frame (378 s for 90 frames). Its use is on degraded or generated 1K clips, which were not available to test. |
| `audio.separate {stems}` | HT Demucs: `vocals`, `rest` (everything but the voice), `drums`, `bass`, `other`; 44.1 kHz stereo, segments of 7.8 s with 25 % overlap as `demucs.apply`. | Candle port equals the PyTorch reference to **95-122 dB SNR** on 20 s of a real recording. 2.25x real time on Metal. Voice + music mix made of a voice recording and generated music: voice **3.6 -> 23.4 dB SDR**, music **-3.6 -> 20.0 dB**. 20.4 s for the 18 s test file, model load included. |
| `clip.matte {mode, background, ratio}` | Robust Video Matting: `matte` = white person on black, `cutout` = the person over a flat colour (green by default); the recurrent state keeps the matte stable. Network on the frame reduced to 512 px (long side), deep guided filter back to full size. | Alpha equals the reference PyTorch model to **+-1 of 255** on 3 consecutive 1024x576 frames, with and without the refiner. 144 frames of 720p in 66 s (0.46 s per frame). Hair and a sheer dress are kept (see below). `foreground` colours are only meaningful where alpha > 0. |
| `media.rushIndex {every}` / `media.rushSearch {query, top, markers}` | SigLIP 2 embeds one frame every `every` seconds (default 2) of the project's video media (index file per media, reused while the file is unchanged); a French or English sentence is ranked by cosine similarity; `markers:true` puts a marker on the media at each hit. | 81 frames of four clips (a 73 s explainer, a 12 s one, a dance clip, colour bars) indexed in 34 s, 0.7 s when up to date. 8 queries (4 English, the same 4 in French: a character holding a light bulb, an employment form, a woman dancing, colour bars): the **right clip is first in all 8**, at the right moment for the two explainers (checked on the frames). Scores 0.12-0.18 for a match, 0.05-0.07 for the rest. |
| `sequence.markBeats` / `cutOnBeats` `model:"beat-this"` | Beat This! (log-mel at 22 050 Hz, 6 transformer blocks, windows of 30 s as the reference), `minimal` peak picking. `bpm` still picks the octave (tracked beats thinned or doubled), `downbeat:true` uses the network's downbeats. | Candle port: logits within 1.1 % RMS of PyTorch (Metal float rounding), identical beats. On the 14 generated drum / chord files of the Beat Edit tests: **no octave error, as with the DSP tracker**, right tempo and 63 of 63 beats, worst frame-snapped error 36 ms against 25 ms (the network works on 20 ms frames). **Real recording** (40 s of music in a dance studio, real footage): the DSP tracker refuses it (« pas de pulsation régulière »), Beat This finds 27 beats at 66.7 BPM (median interval 0.90 s, no ground truth to check against). |
| *Stabilisation L1* (`effects.apply warp_stabilizer` + `result` = 3rd choice) | Warp Stabilizer's result choice « L1 Optimal Path »: the camera path minimises velocity, acceleration and jerk in L1 (Grundmann et al., CVPR 2011, re-implemented from the paper: ADMM on a banded system) inside the crop window of « Crop Less <-> Smooth More » (2-14 % of the frame). | Synthetic shake on a textured scene (known truth): high-pass jitter 23.3 px -> **6.0 px Gaussian, 6.2 px L1**; with a steady pan plus shake the deviation from the clean path is 17.3 px -> **1.5 px Gaussian, 2.5 px L1** (y: 0.74 / 0.41 px). L1 prefers a still camera inside the crop (its pan follows 37 % of the real one at default settings): it does not beat the Gaussian on faithfulness to a pan, it is a different trade. |

**What is not done, and why.** *Whisper, subtitles, translation, DeepFilterNet voice, auto-reframe* belong to the other
two agents of this mission. The LightCraft-only ideas of the list (stuck dust, grouping by person, portrait retouch,
faster denoising) are photo tools and were not touched. *BiRefNet / SAM 2 / MatAnyone* video matting were not attempted: RVM runs in real time on the Mac and matches its reference, MatAnyone's licence
(S-Lab, non commercial) and weights (Google Drive / Baidu) have no checked mirror, BiRefNet and SAM 2 are per-image models
without a temporal state. *Stabilisation from gyroscope data* (Gyroflow is GPL-3.0) is not implemented. The `dbn`
post-processing of Beat This (madmom) is not implemented. `media.rushSearch` reloads the model on every call (2 s warm,
about 20 s the first time); one result per moment is kept (hits within three index steps of a better one are dropped).

## Testing

`cargo test -p filmcraft-extensions` (manifest, store including the shipped-extension offers, archives,
variables, programs, the status line, hostile input), `cargo test -p filmcraft-cli --test cli` (install / list /
validate / pack / remove, the Silence Remover on a generated recording, Beat Edit on generated drums, Copy Pasta,
Storyboard Maker, a program extension), `cargo test -p filmcraft-ui-egui --test extensions_ui` (the menu tree,
run / install / remove / reload, the manager dialog by automation id: `extensions.dialog.*`, the question about a
shipped program extension: `extensions.offer.*`, a transcription run in the background with progress, the locked
sequence and Cancel, a program entry that does not block the window; `FILMCRAFT_TEST_SHOT_DIR=<dir>` also
saves PNGs of the dialogs), `cargo test -p filmcraft-audio-dsp beatgrid` (tempo octave, bar start, refusals),
`cargo test -p filmcraft-engine sequence_extras`, `beats_tests`, `word_captions` and `transcript_tests`; `cargo test -p filmcraft-speech --features whisper,translate` (tokenizer encoding, SentencePiece, catalogue) (`sequence.removeSilence`,
beat commands and their messages, French hesitations).

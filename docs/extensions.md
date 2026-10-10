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

* the ones that only drive FilmCraft's own commands (six of the eight) are installed once; one the user
  removes stays removed (`.bundled-seeded.json` in the extensions folder remembers the offer);
* **an extension that starts an external program (Storyboard Maker, Doublage vocal) is never installed silently.** A
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

`extensions-bundled/` holds eight extensions, in French like their menu entries:

| Extension | Entries | Built on |
|---|---|---|
| **Silence Remover** | Couper les silences · (serré) · (doux) | `sequence.removeSilence`: the sequence mix is rendered through the mixer graph, measured in 10 ms RMS windows, the quiet/loud threshold comes from the recording's noise floor and speech level (the one speech tightening uses, `filmcraft_speech::vad::threshold`), and the quiet stretches of at least `minSeconds` are ripple-deleted on every unlocked track, keeping `keepSeconds` of air next to speech, as one undo step. No transcript, no model. `apply:false` only reports the ranges. |
| **Sous-titres** | Générer les sous-titres · Générer et exporter en .srt · Exporter les sous-titres en .srt | `sequence.transcribe` (built-in Whisper, CPU), `transcript.createCaptions`, `captions.export`. Needs a build with the `whisper` feature and the speech model installed; otherwise the first step fails with "speech-to-text is not available in this build". |
| **Export en un clic** | Export rapide (dernier preset) · YouTube 1080p · YouTube 4K · Vimeo 1080p | `export.quick` with a built-in preset, written next to the saved project (`"path": "${project_dir}/"`, named after the sequence). |
| **Beat Edit** | Marquer les beats · Marquer toutes les 4 beats · Couper sur les beats · Couper toutes les 4 beats | `sequence.markBeats` / `sequence.cutOnBeats` (below). |
| **Copy Pasta** | Copier les attributs · Coller les attributs · Coller seulement les effets · Coller seulement mouvement et opacité · Premier clip vers les autres (tout / effets seulement) | `edit.copy` + `edit.pasteAttributes` (motion, opacity, volume, panner, time remapping, standard effects with their keyframes). The one-step entries select the first selected clip, copy it, paste on the others (`${selection_rest}`) and restore the selection. The Copier / Coller pair needs the clipboard of a session that stays open, i.e. the window. |
| **AutoEdit** | Montage auto (français) · (français, « eux » toujours coupé) · (anglais) · Retirer les hésitations | `sequence.transcribe`, `transcript.removeFillers`, `sequence.removeSilence`: a first cut of talking footage. Each stage is its own undo step and says what it did. French list (`language:"fr"`): euh, euhh, euhhh, heu, heuh, hum, humm, hmm, hmmm, mmh, mm, mmm, hem, ben, bah, beuh; plus **« eux » only when it is a hesitation standing alone** (see below). Whisper often leaves hesitations out or mis-spells them, so only what it wrote can be cut by word; the silences are cut from the audio level whatever the transcript says. |
| **Storyboard Maker** | Exporter une image par plan · Exporter une image par marqueur · Assembler la planche (PNG + PDF) | `exec` entries running `scripts/storyboard.sh` (a Python 3 program): it reads the saved `.fcproj`, and prints `playhead.set` + `file.exportFrame` for each shot (the clips of the first video track that has clips) or marker. The stills (`<project>-storyboard/001.png…` with an `index.json`) come from the Program monitor. The sheet entry lays them out with titles and timecodes (`<project>-storyboard.png` and `.pdf`, `--var columns=4`) and needs the Pillow package. The program only plans: the sheet is built by a second entry because a program cannot read the results of commands that run after it exits. Save the project first. It and Doublage vocal are the bundled extensions that start a program, so they are offered on first launch (a dialog) and never installed silently. |
| **Doublage vocal** | Doubler en anglais · Doubler en anglais avec ma voix · Doubler en français · Choisir la voix de doublage… | `exec` entries running `scripts/dub.sh` (Python 3 standard library + ffmpeg) and the engine command `sequence.placeDub`: see [Voice dubbing](#voice-dubbing-doublage-vocal) below. |

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

### Voice dubbing (Doublage vocal)

*Extensions ▸ Doublage vocal* dubs the selected clip: its speech is translated and spoken again by a
[Voicebox](https://github.com/jamiepine/voicebox) voice (local text-to-speech with zero-shot voice cloning), fitted to the
timing of the original phrases and laid on a **new audio track** while the original voice is lowered. Entries: *Doubler en
anglais*, *Doubler en anglais avec ma voix*, *Doubler en français*, *Choisir la voix de doublage…*. The program is
`scripts/dub.py` (Python 3 standard library) and needs **ffmpeg**; it is an `exec` extension, so it is offered with the
dialog, never installed silently. Save the project first (it reads the `.fcproj`).

**Where the phrases come from** (first match): `--var segments=<.srt|.json>` / `--var translation=<.srt|.json>` (sequence seconds;
this is how any translated subtitle file is plugged in) · a caption track of the sequence already in the target language
(a translated track, such as the one `captions.translate` writes) · the transcript of the selected clip, cut into sentences at
punctuation or a pause of 0.8 s · a caption track in another language. The language of a track is guessed from its text, not
from its label (the Sous-titres entries label a French track `en`). Subtitles are cut by length, not by sense, so cues are
joined until the text ends a sentence and a group holding several sentences is split again (time shared by characters); the
transcript is preferred when it exists because its word times give true boundaries. Hesitations (« euh », « uh ») are dropped
before translation. **Translation**: when nothing is translated yet, `scripts/mt.py` runs the Opus-MT model
(`<data dir>/models/opus-mt-tc-big-<from>-<to>`, installed by `transcript.downloadModel`; any pair that has such a model) in a
Python that has torch, transformers and sentencepiece (`--var python=…`, `--var mt_model=…`). Translations are cached in
`<project>-doublage/translations-*.json`.

**Voice.** Voicebox is started when it does not answer (`~/Applications/Voicebox.app`, `/Applications`, or
`--var voicebox_server=`; data folder `~/Library/Application Support/sh.voicebox.app`; waits up to `start_timeout`=240 s, says
where the log is if it never answers, and stops it afterwards unless it was already running or `keep_server=1`). The voice is
`--var voice=<name or id>`, else the one chosen with *Choisir la voix…* (a macOS list of the profiles Voicebox returns;
remembered per language in `<data dir>/extension-data/doublage-voix/config.json`). *Avec ma voix* takes the profile
remembered as « my voice », or the only profile whose name contains « madson », else asks once. No Voicebox profile is
created, changed or deleted.

**Time fit.** Each phrase has a slot (its original start to end) and a room (the slot plus up to 0.5 s of the pause that follows,
never into the next phrase). The synthesised phrase is trimmed of its silences and measured. Longer than the room: sped up
(`atempo`, pitch unchanged) by at most `max_stretch`=15 %. Shorter than the slot: slowed by at most half of that. Between the two:
left alone. A phrase that still does not fit at +15 % is **reworded shorter** (English contractions and filler words; Opus-MT
n-best candidates when the translator is available) and synthesised again (`shorten=off` disables it); what still overruns is
kept, **not cut**, and reported (« Phrase 3 dépasse de 0,8 s… »). Synthesis stops after `budget_seconds`=3300 s and the
phrases already done stay cached, so running the entry again continues.

**Mix and undo.** The phrases are mixed (ffmpeg, 8 ms fades, limiter) into one file `<project>-doublage/doublage-<lang>-<voice>-<hash>.wav`
that starts at the first phrase, with a `.report.json` (per phrase: slot, room, natural length, speed factor, final length,
overrun, text spoken). One command, `sequence.placeDub {path, time|seconds, name, track?, duck?, note?}`, imports it, adds the
track, places it and writes the volume automation of the original track (`duck`: 120 ms down, 250 ms up around each dubbed stretch,
`original=duck|cut|keep`, `duck_db`=−18) as **one undo step**; any failing step undoes the others. The status bar shows
« 6 phrases doublées (EN, voix Bao EN-US), 0 raccourcie, 6 accélérées/ralenties, 0 dépassement · voix d'origine atténuée de 18 dB ».
The automation is on the **track** of the selected clip: music on the same track is lowered too.

**Measured** (packaged CLI, Voicebox 0.5.0 on a Mac M4, profile *Bao EN-US*, a cloned Qwen voice; `say -v Thomas` French
recordings transcribed by whisper-base; FR→EN by Opus-MT tc-big; the English output re-transcribed phrase by phrase with
Whisper large-v3-turbo and compared with the text that was spoken):

| | 27.4 s recording, 6 phrases (rate 215) | 17.9 s recording, 5 phrases |
|---|---|---|
| natural English length longer than the slot / than the room, before fitting | 2 of 6 / 2 of 6 | 1 of 5 (by 0.01 s) / 0 of 5 |
| after fitting: overruns | 0 | 0 |
| speed factors | 0.925 ×3, 0.957, 1.074, 1.094 | 1.0, 0.925 ×4 |
| length of the dub | 27.29 s (original 27.4 s) | 17.6 s |
| word error rate of the spoken English | 2.2 % (2 / 90) | 3.7 % (1 / 27) |
| translation of the first run (CPU, model load included) | 164 s | |

The errors counted are an apostrophe and a word bled in from the neighbouring phrase. A first measure on the whole file gave
18.5 % on the short recording: Whisper dropped the last phrase, which is in the audio (checked with `silencedetect`); hence the
phrase-by-phrase measure. Nobody listened to the result: the numbers say the text is intelligible and the timing fits, not that
the voice sounds good. Transcription errors travel into the dub (« reches » became « tasks », « publier » became « public »).

**Limits.** No lip-sync (only phrase-level timing). The emotion and intonation are those of the Voicebox model: the cloned Qwen
voice has no emotion control and the voice is not the speaker's unless you clone them (*ma voix* needs a profile; none named
Madson existed on this machine). Proper names and numbers are spoken as the model reads them. One voice per dub: speakers are
not told apart (a diarised transcript is not used). The first launch of Voicebox takes about 45 s, longer on a loaded Mac, and
a cloned-voice phrase can take from seconds to minutes. `atempo` is not a studio time-stretch: beyond ±15 % it is audible,
which is why the limit exists. `ffmpeg` is required. Tests: `cargo test -p filmcraft-engine dub_tests` (undo step, rollback),
`cargo test -p filmcraft-cli --test cli doublage` (the whole entry against a stand-in HTTP server for Voicebox).

## Testing

`cargo test -p filmcraft-extensions` (manifest, store including the shipped-extension offers, archives,
variables, programs, the status line, hostile input), `cargo test -p filmcraft-cli --test cli` (install / list /
validate / pack / remove, the Silence Remover on a generated recording, Beat Edit on generated drums, Copy Pasta,
Storyboard Maker, a program extension), `cargo test -p filmcraft-ui-egui --test extensions_ui` (the menu tree,
run / install / remove / reload, the manager dialog by automation id: `extensions.dialog.*`, the question about a
shipped program extension: `extensions.offer.*`, a transcription run in the background with progress, the locked
sequence and Cancel, a program entry that does not block the window; `FILMCRAFT_TEST_SHOT_DIR=<dir>` also
saves PNGs of the dialogs), `cargo test -p filmcraft-audio-dsp beatgrid` (tempo octave, bar start, refusals),
`cargo test -p filmcraft-engine sequence_extras`, `beats_tests` and `transcript_tests` (`sequence.removeSilence`,
beat commands and their messages, French hesitations).

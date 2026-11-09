# JobFinder

JobFinder checks the job sites for you while your PC is on, matches each post with your CV, and
starts the application: one program for Windows or Linux, in its own window, with your data in one
SQLite file on your PC. Free and open source (BSD 2-Clause). Web page:
https://sasin91.github.io/jobfinder/

## Download

Take the newest [release](https://github.com/sasin91/jobfinder/releases/latest):
`jobfinder.exe` for Windows 11, or `JobFinder-x86_64.AppImage` for Linux. Nothing is installed:
keep the file where you like and double-click it. The release notes say what Windows may ask the
first time. The rest of this README is for building and developing it.

## What is here

| Folder | What |
| --- | --- |
| `app/` | the app (`jobfinder.exe`): the matching, the window, the database. See its README. |
| `laya-server/` | Laya on ONNX Runtime in Rust: the model the app runs in-process, and a `/v1/systemone` HTTP server |
| `assets/` | what the app builds in: the taxonomy index, the Danish postcodes, DejaVu Sans 2.37 for the PDFs, and Figtree and Bricolage Grotesque for the window and the phone app (`fonts`) |
| `extension/jobnet-joblog/` | the Jobnet extension the app ships and you load once in Edge or Chrome |

## Setting it up on Windows

Once, before the first build:

1. Install Rust with [rustup](https://rustup.rs) (the stable MSVC toolchain), and the Visual
   Studio Build Tools with "Desktop development with C++", which Rust's MSVC toolchain links
   with.
2. Clone this repository. Run every `cargo` command from inside `app\`: the repository root has
   no `Cargo.toml`, and cargo would pick up one in a parent folder instead.

Build it:

    cd app
    cargo build --release

The first build downloads ONNX Runtime for Laya (from cdn.pyke.io) and links it into the exe.

### Starting it

    .\target\release\jobfinder.exe

On the first start the app makes an empty database, `%APPDATA%\JobFinder\jobfinder.sqlite`,
writes the Jobnet extension next to it and opens its window on Setup. After that it opens on the
Match screen: an alert while no AI model is in use, and a row each for the AI model, your profile,
Laya, the job scan and the Jobnet extension, saying what is set up and going to what is missing.
Under them are how many matches each list holds and the shortlist's best three, then the match
form.

The app was called Recruiter before. On the default database, the first start of JobFinder moves
the app's files in `%APPDATA%\Recruiter` to `%APPDATA%\JobFinder` (renaming `recruiter.sqlite`
and `recruiter.log` to the new names; any other database there stays), moves the API keys from Credential Manager's `Recruiter` entries into the database's secrets, and replaces the `Recruiter\Job scan` task with `JobFinder\Job scan` at the
same interval (on Linux, the `recruiter-scan` timer with `jobfinder-scan`). It won't start while
Recruiter still runs on that folder; close it first. If you loaded the Jobnet extension, remove it
in Edge and load it again from `%APPDATA%\JobFinder\jobnet-joblog` (its ID stays the same). Delete the old `recruiter.exe` afterwards:
started again, it would make a new empty database in the old folder.

Setup (also under Settings) guides you through six steps and remembers your place:

1. **Welcome:** where your data lives.
2. **Models:** choose local AI or a cloud provider. The Models overview in the sidebar
   explains which model handles each task, its local alternative and why it is suggested.
   On a fresh local setup, **Download and prepare local plan** downloads shared models once
   and saves the task assignments. IBM Granite 4.1 3B Q4_K_M is offered for form answers;
   Qwen3 4B is the compact CV/matching baseline, and Gemma is a job-reading alternative
   when it fits on the GPU. Existing providers and explicit task choices are preserved.
3. **Your CV:** import a PDF, text file or pasted CV. Check the extracted facts before
   matching; edited CV text can replace the profile. Importing a CV does not finish setup.
4. **Jobs and sites:** save your first search, location and remote preference. Choose
   Jobnet, Jobindex, LinkedIn and Indeed individually. Signed-in sites start disabled.
   **Sign in to job sites** opens JobFinder's browser; scans pause when sign-in or a
   person check is needed. OpenAI job evaluation also enables its web search.
   Automatic application/resume writing starts disabled (threshold 101); choose 0–100
   to enable it for matches at or above that score.
5. **First scan:** try one search, matching up to three posts within five active minutes,
   or deliberately skip it. Task manager shows work in progress; Job searches shows
   found posts and individual failures. Configured cloud tasks use your API account.
6. **Schedule:** choose automatic checks or manual scanning, then finish. A new setup
   remains resumable until you finish explicitly. Existing users also review
   onboarding once, with their saved models, profile, searches and matches kept.

**Evaluate again** in Models refreshes hardware and fit estimates without changing
assignments or downloading anything. **Run local check** loads the assigned downloaded
model and checks a small synthetic task fixture, reporting pass/failure and elapsed time.
These checks are compatibility/correctness smoke tests, not proof of overall model quality.
Use the advanced provider/task controls in Language models for manual choices.

Local downloads show progress and can resume after restarting the app when the source
supports ranges and supplies a file validator. Changed files restart cleanly. Model setup
checks disk space, and partial files are kept separate from installed models.

Close its window to stop it. `jobfinder.exe --help` lists the options.

If the app refuses, at start, a database made from an older schema, move `jobfinder.sqlite`
aside and start again.

### The API key

Open Settings, then Language models, and press **Add a key**. Pick OpenAI, Anthropic or a local
server, paste the key (a local server needs its address and a model name instead), and save it.
One key is kept per provider, so an OpenAI and an Anthropic key can be saved side by side. Under
**What the AI runs on**, pick the key each use runs on (evaluating job posts, writing
applications, tailoring résumés) and, under each, the model each of its tasks runs on: reading
job posts, reading CVs and judging the match (evaluating job posts), writing applications, and
tailoring résumés. With this app's own model, job posts are read with Gemma 4 E4B and CVs read
and judged with Qwen3 4B when those files are in `llama\models`; otherwise a task runs on the
key's own model. **Check key** sends one tiny request. The keys are kept in the database, sealed
with a key file beside it (`jobfinder.sqlite-key`), which on Windows only your Windows user can
open: a copy of the database alone gives no key away, and a database moved to another computer
needs its keys saved again. With no model named, OpenAI keys run on gpt-5.4-mini.

### Laya's model

Export the English checkpoint once, as `laya-server/README.md` ("One-time: export the model")
describes. Then put these in `%APPDATA%\JobFinder\laya`, or point `--laya` at another folder:

    laya\rl_agent_config.json   from the checkpoint folder
    laya\tokenizer\             from the checkpoint folder
    laya\laya.onnx              from export.py's out\
    laya\laya.onnx.data         from export.py's out\ (1.7 GB)

Hard links save copying the 1.7 GB file, for example
`New-Item -ItemType HardLink -Path laya\laya.onnx.data -Target <export folder>\out\laya.onnx.data`.
At start the app says whether it found the model. Without it, matches work as before with no
Laya line.

### The app's own model (llama.cpp)

Instead of a key, the app can run a GGUF model itself. Open Settings, then Local model:

1. **Download llama.cpp** fetches the latest release's Windows build for this computer's card
   (CUDA for NVIDIA, Vulkan for every other card, CPU without one) and unzips it into
   `%APPDATA%\JobFinder\llama`, or the folder `--llama` names. A `llama-server.exe` already on
   the PATH (winget's llama.cpp, say) is used instead, and nothing is downloaded.
2. Each offered model (Qwen3 1.7B to 14B, Gemma 4 E4B, Munin Qwen3.5 9B) is weighed against this
   computer's memory and graphics card. **Download** the recommended one into `llama\models`;
   Gemma 4 E4B (reads Danish job posts best) and Munin (writes the best Danish, but heavy) are
   for picking per task, never the recommended one.
3. **Use this model** saves it as **This app's own model (llama.cpp)**, and Language models
   picks what it is used for.

You can also put llama.cpp and `.gguf` files there by hand:

    llama\llama-server.exe      and the DLLs from the same zip
    llama\models\*.gguf         for example Qwen3-8B-Q4_K_M.gguf

On Language models pick **This app's own model (llama.cpp)**. With no model picked the app
picks the biggest model that fits the graphics card's memory with room for its context, or splits
one between the card and memory; Local model lists each model and how it fits this computer. The
server starts on the first AI call (on a free port on 127.0.0.1), writes `llama\llama-server.log`,
and stops when the app does. An NVIDIA or AMD card without CUDA, HIP or Vulkan in the folder is
said on Local model instead of quietly running on the CPU.

### The Jobnet extension

Open Settings, then Jobnet extension, and press **Copy the folder and open Edge's
extensions**. It copies `%APPDATA%\JobFinder\jobnet-joblog` and opens `edge://extensions`. There,
switch on Developer mode, choose **Load unpacked**, paste the folder and press Select Folder. After an app update writes a newer version, press Reload on the extension.

Its toolbar button sends the job post you have open (LinkedIn, Indeed, or any other site) to
JobFinder, as your browser shows it: pick the job search to add it to and press Send. JobFinder
matches it like a post the scan found. The button reads the page only when you press it, and
JobFinder asks the site for nothing. The extension needs JobFinder running: it reaches the app on
http://127.0.0.1:8765, which answers only the extension and the app itself.

### Your phone

A JobFinder app for the phone or a tablet is on its way. It reaches the computer through
Tailscale. The computer's side is here already: Settings, Devices sets up `tailscale serve` and
pairs a device with a QR code. See [docs/phone.md](docs/phone.md).

### From a release

Pushing a `v*` tag builds a GitHub release with `jobfinder.exe` for Windows,
`JobFinder-x86_64.AppImage` for Linux and their `SHA256SUMS`, with `docs/release-notes.md` as its
text (`.github/workflows/release.yml`; run by hand, it only builds the files). Neither is installed: keep the file where you like and
double-click it. Moving it later is fine; at its next start the job scan's schedule follows it.

To remove it, close it and run `jobfinder.exe cleanup` (`./JobFinder-x86_64.AppImage cleanup` on
Linux), then delete the file. The cleanup lists what the app keeps outside its file and asks before
removing it: the job scan's schedule, any API keys still left in Credential Manager (or the keyring)
from before they moved into the database, and the Jobnet extension's files, llama.cpp and Laya next to the database. It then asks separately whether
to delete the database too, with its key file and the keys sealed in it (no unless you type y), and says that the extension itself is removed
in Edge. Give it `--database` for a database elsewhere. When it can't start (the port is taken, the database can't be opened), a small
window says why.

### On Linux (and WSL)

Take `JobFinder-x86_64.AppImage` from a release (it needs glibc 2.38 or newer: Ubuntu 24.04,
Debian 13, Fedora 39 or later), or build it with `cargo build --release` in `app/`; make it executable and run it from wherever you keep it. There is nothing to install:
delete the file to remove it. Tools like AppImageLauncher can add it to the applications menu. The
database is `~/.local/share/JobFinder/jobfinder.sqlite`, and the API keys are sealed in it with
the key file beside it (`jobfinder.sqlite-key`, readable only by you), so no keyring is needed.

The job scan's schedule is a systemd user timer, `jobfinder-scan.timer`, in
`~/.config/systemd/user` (`systemctl --user list-timers` shows it), running the AppImage file
itself. Move the AppImage and the next start points the timer at its new place. Without systemd
(WSL with systemd off, say) the setup says it couldn't be scheduled; on WSL, turn systemd on in
`/etc/wsl.conf` (`[boot]` `systemd=true`). Where FUSE is missing, run the AppImage with
`--appimage-extract-and-run`. `cleanup` removes the timer, as it does the task on Windows. macOS
isn't covered yet.

### Logs

What the app says in its console is kept too. On Windows it goes to `jobfinder.log` next to the
database (`%APPDATA%\JobFinder\jobfinder.log`), which moves to `jobfinder.old.log` once it passes
5 MB. On Linux it goes to the systemd journal: `journalctl --user -t jobfinder` (add `-f` to
follow it). The job scan's timer logs there too, under `jobfinder-scan.service`. Starting the app
again on the same database opens the one that runs.

### Starting it at login

In Task Scheduler, create a task that runs at log on, with the action
`<repo>\app\target\release\jobfinder.exe`, and arguments `--laya ...` or `--llama ...` if you
use them. The window opens at login.

## Developing

One command, on Windows and Linux alike, builds the app, opens its window, and rebuilds and
restarts it on every change (the window opens again):

    cd app
    cargo dev

It needs [watchexec](https://github.com/watchexec/watchexec) once:
`cargo install watchexec-cli --locked`.

- `cargo dev` is an alias (`app/.cargo/config.toml`) for the small program in `app/xtask/`, the
  usual Rust stand-in for npm or Composer scripts. It runs watchexec, which runs a debug build
  (`cargo run`).
- A change to `src/`, `queries/`, `migrations/`, `.sqlx/`, `Cargo.toml`, `../assets/`,
  `../extension/` or `../laya-server/src/` restarts it, as soon as it is built.
- It uses your real database unless you give it another, for example
  `cargo dev --database $env:TEMP\jobfinder-dev.sqlite` (or `/tmp/jobfinder-dev.sqlite` on
  Linux). Other `jobfinder` options go on to the app the same way, and `--port 8766` runs it
  beside a running release build (the Jobnet extension only talks to 8765).

Stop it with Ctrl+C.

JobFinder normally uses one local SQLite database. `--database` is a development/testing
override, not a profile-switching feature. Use a separate test folder to isolate downloaded
models and browser data too. Existing recordings for a renamed test database are copied into
its own folder on startup; the old copies are retained because they may belong to the normal database.

## Trying it out

With a real key saved on Language models, check these by hand after a change:

- [ ] **A moved program:** schedule the job scan, close the app, move `jobfinder.exe` (or the
      AppImage) to another folder and start it from there. The task's action (or the timer's
      service) now runs it from the new folder.
- [ ] **The cleanup:** with the app closed, run `jobfinder.exe cleanup --database` on a test
      database with a scheduled scan. It lists it, removes it on y (the task is gone from Task
      Scheduler), and keeps the database and its key file unless the second question gets a y.
      With the app open it refuses.
- [ ] **The Match screen and Setup:** start with `--database` naming a new file. The window
      opens on Setup, which suggests the app's own model on a PC with a good graphics card, else
      an OpenAI key. Set one up and land back on Setup; read a PDF CV into the profile; pick how
      often to check for new jobs, and `JobFinder\Job scan` shows in Task Scheduler (run it once:
      no console window opens). Finish: the Match screen has no alert, the rows say which model
      and whose profile, and the match form has the CV filled in.
- [ ] **The job scan on the local model:** with the app's own model (or any key but OpenAI's)
      picked for evaluating job posts, make a job search under Job searches, and press Scan now.
      The search lists posts from jobnet.dk and jobindex.dk, matched. Add thehub.io to its Sites
      to skip: the search says the scan skips it.
- [ ] **A page that needs JavaScript:** paste a link to a post whose page shows nothing without
      JavaScript. Edge (or Chrome) reads it headless in the background, and the post is matched;
      `jobfinder.log` says so if no browser was found.
- [ ] **The job scan:** with your OpenAI key picked for evaluating job posts, make a job search
      under Job searches, then press Scan now on the Match screen. The Job scan row says
      "Scanning now", and after a few minutes the last scan's result; the search lists the posts it
      found, matched, with a tailored résumé and application from its score up. Run
      `jobfinder.exe scan` right after: it says every search was scanned less than an hour ago.
      Dismiss a post: its match is in the Rejected list.
- [ ] **The lists:** after a few matches, the Match screen shows them under Shortlist, All and
      the other lists. Show more loads more.
- [ ] **The free match:** paste a job post's text, with evaluating set to no key on Language
      models.
      A match is saved and scored by the free reader.
- [ ] **The AI match:** pick a key for evaluating and paste another post's text. The result shows the
      requirements as the model read them, the score and, with the model in place, the Laya line.
- [ ] **Fetching a job from a link:** paste a link to a public job post (Jobindex or LinkedIn,
      say). The post is read from the page and matched. A link to this PC or the home network is
      refused.
- [ ] **Writing:** on a saved match, write the application, then the tailored résumé. Each one
      appears in its box. Write one again with notes, and the earlier version is listed under it.
- [ ] **The app's own model:** on Local model, download llama.cpp and the recommended model,
      then Use this model; Language models lists it. Press Check (the first check loads the
      model), then run an AI match on it.
- [ ] **PDFs:** Save as PDF on the application, the résumé, the CV and the job post. Each opens as
      an A4 page with the expected file name.
- [ ] **Apply:** open Apply on a match. It shows the email or the employer's site,
      with the PDFs and your profile links.
- [ ] **Rejecting:** Not interested moves a match to Rejected, and Bring back returns it.
- [ ] **Jobnet:** on jobnet.dk, the extension's panel lists your matches, fills in a joblog entry
      and marks the match as on Jobnet.
- [ ] **Keys move in:** start this build on a database whose keys are in Credential Manager.
      The log says how many moved, Language models still shows them, and Credential Manager no
      longer has `JobFinder` entries for that database.
- [ ] **Errors:** rename `jobfinder.sqlite-key` away (the app makes a new one), or switch a use
      off, and try a match.
      The window says why and goes to Language models, and `jobfinder.log` logs it.
- [ ] **Nothing else on 8765:** http://127.0.0.1:8765/match in a browser says the app has no
      web pages.

### The automated checks

These are the checks CI runs on Windows. Run them from `app\` before a push:

    cargo fmt --check
    cargo clippy --workspace --all-targets -- -D warnings
    cargo test --workspace --release

The parity tests compare the matching with a fixed snapshot of expected answers
(`app/tests/fixtures/parity.json`). When a change is meant to move an answer, update the
snapshot in the same commit.

`app/README.md` has more on the tests.

## Licence

BSD 2-Clause: see [LICENSE](LICENSE). `laya-server/` is Apache-2.0, as the laya code it ports;
[THIRD_PARTY.md](THIRD_PARTY.md) lists every part with a licence of its own.

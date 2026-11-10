# First install and model setup

Design proposal based on `main` at `67f7f04`, 8 November 2026. This describes
intended behavior, not features already delivered.

## Goals

A new user can launch JobFinder, configure suitable models, review their profile,
choose jobs and sources, and understand their first scan without consulting the
developer README. Setup can be resumed after closing the app or a failed download.

Linux is a primary platform. Validate Omarchy first, then Ubuntu, Fedora and
openSUSE; retain Windows support. Do not equate a successful Ubuntu CI build with
working installation, GPU acceleration or desktop integration on these systems.

## Baseline before implementation

- `desktop/setup_screen.rs` presents data, AI, CV and scheduling cards. It does
  not create a job search, connect sources or run a first scan.
- `setup/setup_steps.rs::is_first_run` returns false as soon as a profile or saved
  match exists. Leaving setup after CV import can therefore hide unfinished steps.
- `account/task_model.rs` already separates job reading, CV reading, judging,
  application writing, form answers and resume work. Preserve this separation.
- Local defaults include Gemma for job reading and Qwen3 4B for CV reading,
  judging and form answers. Some task defaults are downloaded at first use;
  choosing one base model does not describe the whole download plan.
- `docs/release-notes.md` mentions selecting an already downloaded IBM Granite
  4.1 3B for form answers. The user confirmed this is the roughly 2 GB model
  they used successfully. Granite is not in the download catalog; verify the
  exact GGUF artifact and quantization before adding its download metadata.
- `llama/hardware.rs` detects Windows GPUs through DXGI. Other platforms read
  Linux RAM information but return no GPU.
- `llama/download.rs` selects Windows llama.cpp ZIPs, including on Linux. Linux
  release packaging exists, but this local-runtime setup path needs correction.
- Download pause/resume exists within a running task. Starting `download_file`
  again creates the partial file anew; recovery across application restarts needs
  separate implementation.
- The existing key check validates a tiny structured response; it does not
  measure task quality or representative performance.

## Setup journey

1. **Welcome and system check.** Show detected OS, available storage and compute
   devices. Explain what JobFinder needs next and provide an actionable remedy
   when a prerequisite is missing. Put database paths in secondary details.
2. **Choose how AI runs.** Offer local execution and optional cloud providers.
   Explain which data leaves the computer for the selected configuration.
3. **Review a model plan.** Show one row per task with model, download size,
   expected execution device, recommendation rationale and evidence status.
   Deduplicate shared models when showing total disk and download requirements.
4. **Prepare and test models.** One action installs the correct runtime, downloads
   the approved plan, verifies files, loads models and checks representative
   structured responses. Show progress per stage and keep failures recoverable.
5. **Import and review CV.** Support PDF, text and paste. Review extracted facts
   and experience before using the profile; allow manual correction.
6. **Define desired jobs.** Create the first saved search: roles/keywords,
   location, remote preference and existing matching options. Keep generated
   application settings explicit rather than an unnoticed side effect.
7. **Choose job sites.** Offer individual source controls, explain which require
   sign-in or a cloud provider, and show connected, available, skipped or blocked
   states. Use the existing source integrations and surface their actual limits.
8. **Try a first scan.** Run a bounded scan with cancellation and visible stages.
   Show a first match with its reasons, or explain whether no posts were found,
   posts failed to load, matching failed, or none met the criteria. Offer a direct
   correction for the cause instead of an unexplained empty dashboard.
9. **Choose a schedule.** Offer manual scanning as a completed, valid choice.
   Verify the scheduled command and explain when the machine must be on and the
   user logged in. Finish with a compact readiness summary.

Store explicit, versioned onboarding progress per database, including deliberate
skips. Derive readiness from current configuration too: a previously completed
step can need attention after its model is removed. Never infer completion solely
from CV import. Every user reviews onboarding once, including existing populated
databases. Their saved models, profiles, searches and matches remain available;
explicit completion returns subsequent launches to the dashboard.

## Task-specific model recommendations

Use the existing six task identities as the execution contract. Recommendations
must be a shared service consumed by setup, Language models and runtime routing,
so the UI's proposed plan and the model actually used cannot drift apart.

Evaluate candidates for the work they will do:

| Task | Required evidence |
| --- | --- |
| Read job posts | Requirements, dates and application instructions; Danish and English |
| Read CVs / import profile | Accurate facts and dates, missing information preserved |
| Judge a match | Requirement verdict accuracy and explanations grounded in inputs |
| Write applications | Grounded claims, requested language and usable writing |
| Tailor resumes | Factual consistency and valid output structure |
| Answer job forms | Correct field values, valid choices and no invented candidate facts |

Include IBM Granite 4.1 3B as a form-answer candidate, backed by the user's
successful experience. Preserve existing selections. Verify the precise artifact
and evaluate its form-answer quality before declaring it the best default;
success on forms does not establish suitability for every other task.

Keep catalog evidence explicit: model/version, quantization, supported task,
languages, benchmark version and known limitations. File size alone is not a
quality threshold. Recommend the least resource-intensive candidate that meets
the task's quality bar and the user's speed/quality preference; consider the cost
of switching between models on a small GPU. Offer a single-model plan where its
measured tradeoffs are acceptable.

Separate two kinds of evaluation:

- A maintained, task-specific benchmark establishes quality using representative
  fixtures and human-reviewed expected results, including form-answer cases.
- A short on-device check establishes compatibility, schema handling, load time,
  response time and observed memory/device behavior. It must not label a model
  high quality just because it returned valid JSON quickly.

Before downloading, account for model files, runtime, context memory, temporary
extraction space and disk reserve. Before applying a plan, show required downloads
and changes. Keep manually selected task models unless the user selects their
replacement. Unsupported or failed task models should surface a recovery choice,
not trigger an unannounced cloud request or another large download.

## Model overview and re-evaluation

Make **Models** easy to find from the main navigation and setup. The overview
shows, for each task:

- Which model is currently used, including its provider and quantization.
- Why it was chosen: task-quality evidence, language, speed and hardware fit.
- Whether it is downloaded and tested, and which device/backend runs it.
- Alternatives and their tradeoffs, with advanced manual selection available.

Provide a prominent **Evaluate again** button and show when the last evaluation
was performed. It refreshes hardware and runtime capabilities, compares current
assignments with the catalog and offers local checks. Explain the work and any
downloads before starting an expensive evaluation. Show current choices beside
proposed replacements and let the user apply selected changes.

Persist a versioned assessment containing the observed hardware capabilities,
runtime/backend and catalog/evaluation versions, per-task choices, measurements
and assessment time. There is no automatic hardware-change detection, startup
comparison, change notification or dismissal tracking in this scope. A brief
overview hint can say: "Changed your hardware or want to compare models? Evaluate
again."

Re-evaluation must preserve manual choices until replacements are applied. It
must not delete models or silently move work to a cloud provider. If an assigned
model fails at runtime, show that failure and a route back to the overview.

## Linux delivery

Select runtime assets by OS, architecture and validated backend, with independent
archive handling for Linux. Never fetch Windows DLLs/EXEs for Linux. Confirm the
actual upstream archive layout, executable permissions and shared-library lookup.
Unsupported architectures or missing release assets need an explicit recovery path.

Detect GPUs through vendor-independent enumeration and backend probes. Linux DRM
sysfs can identify adapters; use available vendor tools/backend queries for usable
memory and acceleration. Handle multiple adapters, missing utilities, inaccessible
devices and integrated/shared-memory GPUs. Unknown VRAM must remain unknown;
never treat all host RAM as dedicated VRAM or claim GPU acceleration from a vendor
name alone. Bound probe time and keep setup usable when detection fails.

Omarchy acceptance must cover its Wayland/Hyprland session, file picker portals,
GPU driver/backend availability, application launching, source sign-in browser,
notifications, user timers and reboot/resume. Reuse capability checks across
Ubuntu, Fedora and openSUSE; distro-specific instructions are remedies, not a
substitute for capability detection. Never run package-manager commands silently.

Keep AppImage as the initial distribution format while validating its actual
glibc baseline, FUSE and extraction fallback on each supported release. State
tested distribution versions in release notes. No support claim should precede
an installation smoke test on that distribution.

### Testing with the available Windows desktop

Use WSL2 for the first Linux development and integration loop. Record the guest
distribution, WSL version, graphics exposure and whether systemd is enabled.
Validate Linux builds, archive selection/extraction, model downloads and recovery,
CPU inference, configuration persistence and user timers where systemd is enabled.
Exercise the desktop through WSLg where available, including file picking and
source sign-in. Test GPU execution only when the host driver, WSL exposure and
chosen backend actually support it; measure the backend in use rather than
assuming native Linux GPU behavior.

WSL may expose a GPU without the native Linux DRM sysfs layout. Treat this as a
separate capability-discovery case and use available backend/vendor queries;
absence of `/sys/class/drm` adapters alone must not establish CPU-only hardware.

WSL is useful evidence for these paths, but it does not validate native Omarchy's
Hyprland session, portals, driver setup, AppImage launch integration or normal
desktop lifecycle. Keep native Omarchy acceptance pending until a test machine,
live environment or VM is available. A VM can validate installation and desktop
integration; accelerated performance still needs representative GPU access.
Then repeat distribution-specific acceptance on Ubuntu, Fedora and openSUSE.

## Delivery order and acceptance

1. Correct Linux runtime installation and GPU/capability detection. Test asset
   selection, archive installation, unknown memory and CPU fallback. Complete a
   fresh Omarchy installation before claiming the local model path works there.
2. Build the shared task-plan service, model overview and evaluation records. Add
   Granite 4.1 3B with verified download metadata and form-answer fixtures. Test
   quality gating, hardware fit, manual overrides and actual runtime routing.
3. Add resumable downloads and one-action model preparation with staged checks.
   Test interruption/restart, changed remote files, insufficient disk, failed
   extraction, absent drivers and loading failures without corrupting working files.
4. Add persisted onboarding progress, search/source configuration and a first
   scan. Test restart after each step, deliberate skips, existing databases,
   blocked sign-in, offline use and a scan with no matches.
5. Add **Evaluate again**, with a review of proposed changes. Test refreshed
   capabilities, probe failure, catalog changes and preservation of task overrides.
6. Expand release validation from Omarchy to Ubuntu, Fedora and openSUSE, using
   pinned, documented versions and testing both CPU-only and accelerated paths.

## Implementation status

The repository now implements a six-stage setup UI with saved stage/choices,
explicit completion, search/source configuration, optional first scan and manual
scheduling. Migration 0018 starts every database in onboarding, including existing
users, without resetting their saved data.
Migration 0018 directly creates typed `onboarding` and `model_assessments`
columns, with per-task measurements in `model_checks`. Schema migrations track
storage changes; `evaluation_version` identifies the test criteria.
Constraints enforce flags, step ranges and complete optional GPU information.
Assessment metadata and its child checks are saved in one transaction.

A fresh desktop launch asks where to keep the database and app data before
opening SQLite. Setup starts with the database path and a picker for the local
models folder. Both choices carry over to subsequent launches.

Setup's Models step embeds a simpler version of the Models overview: detected
hardware, six task selectors, recommendations with reasons and fit estimates,
shared-model download totals, progress/retry, and optional local checks. Cloud
and external provider configuration stays behind an advanced-choices link.
Users can adjust every task before choosing **Use these models**. This saves the
explicit choices and automatically downloads their missing local files and
runtime. Continue is available once the choices are saved. The bottom status
bar shows background progress and links to Models for retries. Reading the CV
and running the trial scan require the models and runtime to be ready.

Models in the main navigation uses the same task selectors and download action,
with the complete catalog and folder controls below. Evaluate again is explicit
and preserves settings. Granite 4.1 3B Q4_K_M is recommended for forms, Qwen3 4B
for general work and Gemma for job reading when it fits the GPU. Existing cloud
providers and manual task assignments are preserved until explicitly edited.
Advanced provider/task changes also prepare their selected local assignments
automatically. First-run provider routing and all six task choices commit in one
transaction before downloads begin.

Bundled runtime integrations implement the `ModelRuntime` trait in separate files
under `account/model_runtime/`. The registry supplies trait objects; the planner
uses their model resolution, recommendations, availability, size estimates,
validation, preparation and loading operations. `llama_cpp.rs` owns the current
implementation. Adding a managed runtime requires an implementation and registry
entry rather than a new enum variant or runtime branches in the planner.

Linux runtime asset selection supports x86_64 and aarch64 CPU/Vulkan archives and
compatible available CUDA 12.8 archives. Installation preserves executable modes
and relative shared-library links. Native DRM and NVIDIA/WSL inventory are
implemented; unknown GPU memory stays unknown. Runtime loading checks still
establish whether a selected backend actually works with installed drivers.
Downloads retain URL/validator recovery metadata across restart and check ranges,
space, transfer length and the GGUF signature before accepting model files.

The maintained comparative quality benchmark remains follow-up work. Current
local checks use versioned synthetic fixtures and must not be presented as a
comprehensive quality ranking. Profile corrections currently use edited CV text
and re-import; a direct structured profile editor is not implemented. Native
Omarchy, WSL GPU, Windows and distribution-specific installation acceptance still
need the appropriate machines; see linux-validation.md for the manual checklist.

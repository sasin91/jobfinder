# Linux setup validation

Linux is a primary target: Omarchy first, followed by Ubuntu, Fedora and openSUSE.
The app's AppImage and downloaded runtime have their own system-library requirements;
a successful Rust build is not a distribution support claim.

## Start with WSL2 on the Windows desktop

Record `wsl --version` and `wsl -l -v` in Windows. In the guest, record
`cat /etc/os-release`, `uname -m`, the GPU/backend detected in Models and whether
`systemctl --user` works. WSLg is needed for the app window and file picker.

1. Use a fresh test database: `jobfinder --database /path/to/test.sqlite`.
2. Launch setup, advance through a few steps, close and restart. It should resume
   at the saved stage. Importing a CV must leave setup unfinished.
3. Open Models. Verify RAM/GPU information. Under WSL, NVIDIA may be exposed through
   `/usr/lib/wsl/lib/nvidia-smi` even without native DRM cards. Unknown memory must
   be labelled unknown, and no GPU acceleration should be claimed from inventory alone.
4. Review per-task choices and the total missing model download size. Prepare the
   local plan. It must fetch Linux archives, never Windows ZIPs or DLLs.
5. Interrupt a model download, restart, and retry. With a validator/range-capable
   source it should continue from the partial file. Check a simulated offline or
   low-space failure gives a remedy and never leaves a partial installed model.
6. Run local checks for assigned models. Save elapsed times, pass/fail and the
   runtime log; verify the backend actually used. A fixture pass is not a full
   quality evaluation. Compare Granite form answers with familiar real forms.
7. Set a custom task model. Evaluate again. It must preserve that choice and all
   provider assignments, with no automatic downloads or cloud calls.
8. Import/review a test CV, save a search and choose its sources. Connect optional
   signed-in sites in JobFinder's browser. Keep automatic document writing disabled
   initially; no provider credentials belong in test reports or screenshots.
9. Try the bounded scan; inspect found posts, failures and match explanations.
   Stop a trial and confirm it releases its lock. Also test no results and offline
   behavior. The main scan can process remaining saved posts later.
10. Choose manual scanning and finish; restart to the dashboard. Separately test
    timer registration where systemd is enabled and verify its command points at
    this database and executable. Without systemd, manual setup must still work.

## When native Linux is needed

Use a separate drive, live environment or laptop once a candidate AppImage has
passed the WSL loop. Keep the Windows installation intact. A spinning disk is
suitable for installation checks but model load times should be labelled as HDD
measurements. Record laptop GPU/RAM before comparing performance.

On native Omarchy, repeat the checklist and verify Hyprland/Wayland launch,
desktop portals and file selection, source sign-in windows, notifications,
AppImage FUSE/extraction fallback, user timers, reboot and suspend/resume.
GPU/driver behavior and native desktop integration require this environment;
WSLg cannot validate them. A VM helps with installation and portals but needs
representative GPU access for performance conclusions.

Then repeat distribution-specific checks on documented Ubuntu, Fedora and openSUSE
versions. Do not mark native/GPU cases passed from this cloud environment alone.

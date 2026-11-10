JobFinder checks the job sites for you while your PC is on, matches each post with your CV, and starts the application. Free and open source (BSD 2-Clause).

## New in 0.1.3

- **Startup screen.** See JobFinder's progress while the desktop app opens.
- **Storage and model downloads.** Choose where to keep your data and models during setup. Local model downloads continue in the background while you work through setup, with progress in the status bar.
- **Setup progress and CV privacy.** See which setup steps are still pending from the dashboard. The dashboard and setup explain when a hosted provider, remote model server, or cloud fallback can receive your CV and profile data.
- **Direct website downloads.** The JobFinder website offers the latest Windows and Linux builds directly and selects a download for your desktop platform when it can.

## Download

- **Windows 11:** `jobfinder.exe`. Keep it where you like and double-click it. Nothing is installed.
- **Linux:** `JobFinder-x86_64.AppImage` (Ubuntu 24.04, Debian 13, Fedora 39 or newer). Make it executable and run it.

`SHA256SUMS` lists each file's checksum.

**If Windows says MSVCP140.dll or VCRUNTIME140.dll was not found,** install Microsoft's [Visual C++ Redistributable (x64)](https://aka.ms/vs/17/release/vc_redist.x64.exe) and start JobFinder again. Most PCs have it already.

**Windows may warn you the first time.** The program isn't signed with a paid certificate yet, so Edge can say it "isn't commonly downloaded" (choose **Keep**), and SmartScreen can say "Windows protected your PC" (choose **More info**, then **Run anyway**).

## The first start

JobFinder opens on **Set up JobFinder**. Choose where to keep your files, then follow the steps to select models, import your CV, and save your first job search. You can try or skip the first scan, then choose automatic checks or manual scanning before finishing.

The Jobnet extension for Edge or Chrome is under Settings, Jobnet extension.

## Removing it

Close JobFinder and run `jobfinder.exe cleanup` from a terminal: it lists what it keeps outside its own file (the scan's schedule, downloads, the database) and asks before removing anything. Then delete the program file.

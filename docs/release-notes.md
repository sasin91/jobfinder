JobFinder checks the job sites for you while your PC is on, matches each post with your CV, and starts the application. Free and open source (BSD 2-Clause).

## New in 0.1.2

- **Resumable setup.** Review your models, import your CV, set up your first job search, and try a short scan before choosing a schedule. JobFinder saves your place and keeps your existing models, profile, searches, and matches when you upgrade.
- **Models by task.** See recommendations and choose which model handles each task directly in Setup. JobFinder automatically downloads selected local models, shares downloads across tasks, and can run a local check on a downloaded model.
- **Model download recovery.** See download progress and resume interrupted downloads when the source supports it. JobFinder checks available disk space before downloading.
- **Job searches.** Set your location and remote preference, choose job sites individually, and save the search by continuing to the next step. Signed-in sites and automatic application drafting start disabled for new searches created in setup.

## Download

- **Windows 11:** `jobfinder.exe`. Keep it where you like and double-click it. Nothing is installed.
- **Linux:** `JobFinder-x86_64.AppImage` (Ubuntu 24.04, Debian 13, Fedora 39 or newer). Make it executable and run it.

`SHA256SUMS` lists each file's checksum.

**If Windows says MSVCP140.dll or VCRUNTIME140.dll was not found,** install Microsoft's [Visual C++ Redistributable (x64)](https://aka.ms/vs/17/release/vc_redist.x64.exe) and start JobFinder again. Most PCs have it already.

**Windows may warn you the first time.** The program isn't signed with a paid certificate yet, so Edge can say it "isn't commonly downloaded" (choose **Keep**), and SmartScreen can say "Windows protected your PC" (choose **More info**, then **Run anyway**).

## The first start

JobFinder opens on **Set up JobFinder**. It shows where your data lives, then guides you through model selection, CV import, and your first job search. You can try or skip the first scan, then choose automatic checks or manual scanning before finishing.

The Jobnet extension for Edge or Chrome is under Settings, Jobnet extension.

## Removing it

Close JobFinder and run `jobfinder.exe cleanup` from a terminal: it lists what it keeps outside its own file (the scan's schedule, downloads, the database) and asks before removing anything. Then delete the program file.

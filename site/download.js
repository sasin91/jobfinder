const windowsDownload = "https://github.com/sasin91/jobfinder/releases/latest/download/jobfinder.exe";
const linuxDownload = "https://github.com/sasin91/jobfinder/releases/latest/download/JobFinder-x86_64.AppImage";
const isWindows = /Windows NT/.test(navigator.userAgent);
const isLinux = /Linux/.test(navigator.userAgent) && !/Android/.test(navigator.userAgent);

for (const link of document.querySelectorAll("[data-desktop-download]")) {
  if (isLinux) {
    link.href = linuxDownload;
    if (link.id === "desktop-downloads") {
      link.textContent = "Download for Linux";
    } else {
      link.setAttribute("aria-label", "Download JobFinder for Linux");
    }
  } else if (!isWindows && link.id !== "desktop-downloads") {
    link.href = "./#desktop-downloads";
    link.setAttribute("aria-label", "Choose a JobFinder desktop download");
  }
}

const otherDownload = document.querySelector("[data-other-desktop-download]");
if (isLinux && otherDownload) {
  otherDownload.href = windowsDownload;
  otherDownload.textContent = "Windows (EXE)";
}

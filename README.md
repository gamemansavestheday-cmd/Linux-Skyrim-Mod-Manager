# Linux Skyrim Mod Manager (LSMM)

LSMM is a native Linux mod manager for Skyrim Special Edition. It is designed to be lightweight, fast, and to avoid the complexity and overhead of virtual filesystems (VFS) under Wine/Proton. Instead, it manages your mods using native Unix symlinks.

## Features

* **Symlink-Based Deployment:** Mods are kept in a separate folder and linked directly into your Skyrim Data directory. The app tracks links in a manifest file, allowing clean deployments and complete rollbacks.
* **Profile Management:** Set up separate profiles for different playthroughs. Each profile maintains its own mod list, enabled states, and load order.
* **Conflict Analyzer:** Scans active mods in a background thread to identify duplicate loose files and displays which mod's files will write over others.
* **Proton INI Editor:** Locates and loads your virtual skyrim.ini and skyrimprefs.ini files inside your Steam/Proton prefix for direct editing.
* **Smart Manual Importer:** Scans your Skyrim Data folder for loose files and manual mods, automatically sorting them into named mod folders inside the manager while keeping core vanilla files untouched.
* **Background Downloader:** Downloads direct links and extracts archives in a separate thread, keeping the GUI responsive.

## Requirements

The manager relies on native Linux system commands for extraction. Ensure you have the following installed on your system:
* `curl` (for web downloads)
* `unzip` or `p7zip` (for archive extraction)

## Installation and Usage

1. Download the latest binary from the Releases tab.
2. Place the binary in the directory where you want to store your mods.
3. Make the binary executable:
   ```
   chmod +x lsmm 
   ./lsmm
    
  On the first run, the app will generate a mods folder next to the executable. Place your extracted mod folders there, or use the "Global Mod Stash" tab to import zip archives.
    Configure your Skyrim Data path and the path to your Proton prefix's plugins.txt file in the sidebar.

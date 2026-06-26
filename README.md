<h1 align="center">lowcat</h1>

<p align="center">
  Lightweight and cross-platform sound library app that runs locally.<br>
  Forget about using file manager!
</p>
<p align="center">
  Powered by
  <a href="https://www.gpui.rs/">gpui</a> library.
</p>
<img alt="yes, i love zed's UI" src="https://github.com/user-attachments/assets/833b1164-188c-4c0e-a256-a589c5fc5ac0" style="width:100%; height: auto;" />

## Features

- Native
- Multi-tag filtering
- Multi-file-extension support
- Built-in YouTube downloader
- Built-in file converter

## Getting started
1. Go to [Releases](https://github.com/debuxxed/lowcat/releases)
2. Download the installah (`.msi` for Windows, `.dmg` for MacOS)
3. Run it

## How it works?
Currently the app has 2 _categories_: _Music_ and _SFX_ (displayed as tabs on the titlebar).
Each category must have its source — the actual folder that it watches.

The files of each category can have _tags_ — those, and other data are stored in local SQLite database.

If there are files with the same name, but different extension — they are grouped into _stems_, which are displayed as rows in UI.

## Contribute
You can help A LOT by spreading a word, or document the app (YouTube tutorials, guides).

If you prefer donating instead, you'd get to suggest features and have quick bugfixes.

Join our <a href="https://discord.gg/MPDehxzDHT">discord</a> btw!

## Why?

I've been editing for many years, 
and always struggled with certain problems:

**There's no adequate sound library that runs locally.**

- They are either proprietary,
- or, don't have multi-tag filtering
- or, are subscription-based websites

This means you can't add your own sounds to library and tag it
– now you have to rely on a third-party website.

Not only that, but now you also lose your precious time, 
because of extra steps to download the sound files.

...so, i've decided to break this vicious cycle, 
and therefore, make this app.

# Mod Menu
![Screenshot of the Mods screen, showing a list of a few mods on the left side below a search bar and filters button, where Mod Menu is selected. On the right side of the screen, it shows more details about the mod, such as authors, a description, links, credits, and a button to configure the mod.](https://cdn.modrinth.com/data/mOgUt4GM/images/155dd2b006883b168b1279ec0ff21e753946518b.png)

Mod Menu lets you view the mods you have installed and, if supported by the mod, enables quick and easy access to the mod's config screens.

Mod Menu also supports some more advanced features, such as translatable mod names and descriptions, support for [QuickText formatting](https://placeholders.pb4.eu/user/quicktext/) in mod descriptions thanks to [Patbox](https://ko-fi.com/patbox)'s [Text Placeholder API](https://modrinth.com/mod/placeholder-api), filters library mods out from regular mods, a mod update checker for mods hosted on Modrinth or that provide their own update sources, and deep configuration for all the features we provide.

### Supported Platforms
Mod Menu is currently available for Fabric or Quilt on Minecraft: Java Edition 1.14 or newer.

## Developers
Mod Menu includes a number of APIs for developers to improve how their mod appears in Mod Menu. These come in the form of language keys, JSON metadata, and even a Java API.

### Translation API
You can translate your mod's name, summary, and description all without touching any Java code. Simply add translation keys in the supported format to any language you'd like.

<details>
<summary>Translation API Documentation</summary>

Here's an example of Mod Menu's translations into Pirate Speak. To create your own, simply replace `modmenu` at the end (***NOT*** the one in the beginning) of the translation key with your own mod ID, for example `modmenu.descriptionTranslation.traverse`.

`en_pt.json`
```json
"modmenu.nameTranslation.modmenu": "Menu o' mods!",
"modmenu.descriptionTranslation.modmenu": "Menu o' mods ye installed matey!",
"modmenu.summaryTranslation.modmenu": "Menu o' mods ye installed matey!"
```

> The summary translation is redundant here and does not need to be included because it's the same as the description, but it was included to show that you may translate the summary (a short, one-sentence description of the mod) separately from the description, even in English!

</details>



### Fabric Metadata API
There's a number of things you can add just with metadata in your `fabric.mod.json`.

All of these are added to a custom block in your `fabric.mod.json` for Mod Menu's metadata. Here's an example usage of many of the features this API provides:

`fabric.mod.json`
```json5
{
  ...
  "custom": {
    "modmenu": {
      "links": {
        "modmenu.discord": "https://discord.gg/jEGF5fb"
      },
      "badges": [ "library", "deprecated" ],
      "parent": {
        "id": "example-api",
        "name": "Example API",
        "description": "Modular example library",
        "icon": "assets/example-api-module-v1/parent_icon.png",
        "badges": [ "library" ]
      },
      "update_checker": true
    }
  }
}
```

<details>
<summary>Fabric Metadata API Documentation</summary>

#### Badges (`"badges": [ ]`)
While the `Client` badge is added automatically to mods set as client-side only (set `"environment": "client"` in `fabric.mod.json` to do this.), other badges such as the `Library` and `Deprecated` badges require definition here.

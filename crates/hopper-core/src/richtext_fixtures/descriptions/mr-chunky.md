<h2>Chunky</h2>

Pre-generates chunks, quickly, efficiently, and safely

<h3>Features</h3>

- Start one or several chunk generation tasks at the same time (if you want to leverage your CPU more)
- Pause chunk generation tasks, saving them for later
- Shows detailed information such as chunks processed, percent to completion, ETA, chunk processing rate, etc
- Custom world [shapes](https://github.com/pop4959/Chunky/wiki/Shapes), and optional world border support with [ChunkyBorder](https://modrinth.com/plugin/chunkyborder) installed

<h3>Permissions</h3>

In order to use chunky you will need to either:
- Be an operator on a dedicated server. To become an operator, someone with access to the server must run the command *op <username>* from the server console.
- Be in a singleplayer world. In older versions of chunky you may also need to enable cheats for the world. This can be done during world creation, or by using the "Open to LAN" button.

<h3>Commands</h3>

<h4>Task Management</h4>

**chunky start** *Starts a new chunk generation task from the current selection*
</br>
**chunky pause** *Pauses the current chunk generation tasks, and saves progress*
</br>
**chunky continue** *Continues running current or saved chunk generation tasks*
</br>
**chunky cancel** *Stops the current chunk generation tasks, and cancels progress*

<h4>Selection</h4>

**chunky world [world]** *Sets the currently selected world*
</br>
**chunky shape <shape>** *Set the [shape](https://github.com/pop4959/Chunky/wiki/Shapes) to generate*
</br>
**chunky center [<x> <z>]** *Sets the current center block location*
</br>
**chunky radius <radius>** *Sets the current radius*
</br>
**chunky worldborder** *Set the center and radius to match the world border in the selected world*
</br>
**chunky spawn** *Set the center to the spawn point*
</br>
**chunky corners <x1> <z1> <x2> <z2>** *Set the selection by corner coordinates*
</br>
**chunky pattern <pattern>** *Set the preferred generation [pattern](https://github.com/pop4959/Chunky/wiki/Patterns)*
</br>
**chunky selection** *Display the current selection*

<h4>Miscellaneous</h4>

**chunky silent** *Toggle displaying update messages*
</br>
**chunky quiet <interval>** *Set the quiet interval in seconds for update messages*
</br>
**chunky progress** *Display pre-generation progress in-game for all tasks*
</br>
**chunky reload** *Reloads the configuration*
</br>
**chunky trim** *Delete chunks outside selection*

<h3>Examples</h3>

**Generate chunks in a square centered at 0,0 in the overworld with a 1000 block radius**
```
chunky radius 1000
chunky start
```
**Generate chunks in a circle centered at spawn in the nether with a 1000 block radius**
```
chunky world the_nether
chunky shape circle
chunky spawn
chunky radius 1000
chunky start
```
**Generate chunks inside a world border centered at 0,0 with a 10000 block radius**
```
worldborder center 0 0
worldborder set 20000
chunky worldborder
chunky start
```

<h3>Links</h3>

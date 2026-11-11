![Entity Culling Banner](https://tr7zw.github.io/uikit/banner/header_entity_culling.png)

<p align="center" style="text-align: center;">
  <a href="https://discord.gg/caVV5eXekm"><img src="https://tr7zw.github.io/uikit/social_buttons_icon/Discord-Button-64.png" alt="Discord" style="margin: 5px 10px;"></a>
  <a href="https://github.com/tr7zw/EntityCulling"><img src="https://tr7zw.github.io/uikit/social_buttons_icon/Github-Button-64.png" alt="GitHub" style="margin: 5px 10px;"></a>
  <a href="https://modrinth.com/mod/entityculling"><img src="https://tr7zw.github.io/uikit/social_buttons_icon/Modrinth-Button-64.png" alt="GitHub" style="margin: 5px 10px;"></a>
  <a href="https://www.curseforge.com/minecraft/mc-mods/entityculling"><img src="https://tr7zw.github.io/uikit/social_buttons_icon/Curseforge-Button-64.png" alt="GitHub" style="margin: 5px 10px;"></a>
</p>

<br>![Divider](https://tr7zw.github.io/uikit/divider_faded/Divider_01.png)

<img src="https://tr7zw.github.io/uikit/headlines/large/About.png" alt="About" style="margin: 5px 10px;">

Minecraft skips rendering things that are behind you, so why is it rendering everything that you still can't see because of walls or ceilings in the way?

This mod introduces **asynchronous path-tracing** to efficiently determine what's actually visible to the player. Using other available CPU cores, it calculates line-of-sight visibility in real time and eliminates unnecessary draw calls/processing for hidden block entities and entities.

This can drastically improve the frame rate, depending on the number of entities and the position in the world.

<br>![Divider](https://tr7zw.github.io/uikit/divider_faded/Divider_01.png)

<br>![Features](https://tr7zw.github.io/uikit/headlines/large/Features.png)

### Asynchronous Path-Tracing

- Uses spare CPU threads to calculate visibility
- Runs alongside the main game thread without blocking (every few ticks, required data will be collected on the main thread)
- Updates visibility data in real time, keeping pop-ins to a minimum

### Entity Tick Optimization

- Reduces client impact from entities that are not visible
- Fully configurable and compatible with most mods
- No impact on server-side simulation, farms, or mob behavior

### Fully Configurable

- All features of the mod can be toggled on/off in the config screen
- Whitelist entities and block entities that should not be culled or tick-culled
- When playing with fast graphics or custom leaves, there is an option to treat leaves as solid blocks for Entity Culling. This can help to increase the performance in forested areas

<br>

[![Essential](https://tr7zw.github.io/uikit/banner/essential_1.png)](http://essential.gg)<br>

<br>![Divider](https://tr7zw.github.io/uikit/divider_faded/Divider_01.png)

<br>![Compatibility & Dependencies](https://tr7zw.github.io/uikit/headlines/medium/Compatibility%20&%20Dependancys.png)

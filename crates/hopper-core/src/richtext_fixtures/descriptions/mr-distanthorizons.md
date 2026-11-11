<center>
  <img width="420" src="https://media.forgecdn.net/attachments/1111/872/distant-horizons-png.png" alt="Distant Horizons">
</center>

<br>

Distant Horizons allows you to achieve extreme (256+) chunk render distances at a low performance cost. <Br>
Now you can finally enjoy that mountain top lookout tower.

<br>

![Mountain top villager](https://cdn.modrinth.com/data/uCdwusMi/images/fd8a1d3aad9b543bc07967478e7f9e269764c4ac.webp)
### <center> This photo was taken with a Minecraft render distance of 8 and a mod distance of 512. </center>

<br>
 
# FAQ:

### **Q: Forge or Fabric?** <br>
A: MC 1.20.6 and newer - NeoForge and Fabric are both in the same jar. <br>
MC 1.20.4 and older - Forge and Fabric are both in the same jar. 

<br>

### **Q: Shaders?** <br>
A: Yes, but only with certain shaders. <br>
- Optifine: DH partially works with forward rendered shaders.
- Iris (and equivalent ports): DH 2.1.0+ only works with Iris 1.7+ and only with shaders built with DH support in mind. 
  - DH 2.0 and older don't work with Iris.

<br>

### **Q: Does this work with player created structures?** <br>
A: Yes.

<br>

### **Q: Does this render using my GPU?** <br>
A: Yes. <Br>
Distant Horizons uses OpenGL or Vulkan for graphics rendering.

<br>

### **Q: Is there multiplayer support?** <br>
A: Yes

**client side only:** <br>
If you only have DH installed on your client LODs will be saved between sessions, although you have to explore the world to load them.
 <br>

**Server and Client:** <br>
If DH is installed on the server and client, LODs will automatically be sent to each connected user that has DH installed and has `Distant Generation Enabled` set to true, disabling distant generation on the server will prevent LODs from being sent to the users.

<br>

### **Q: My game is stuttering, help!** <br>
A: Try each of the following steps:
1. Use a concurrent garbage collector like ZGC for Java 21+ or Shenandoah for Java 8-17
   - This can be done via your JVM arguments (args)
2. Increase Minecraft's allocated RAM
   - For CPUs with 4-8 cores: try allocating 6-8 GB
   - For CPUs with 10+ cores: try allocating 12-16 GB
3. Lower DH's CPU load in the DH config menu
4. Disabling DH's "Distant Generation" in the DH config menu
5. Lower DH's "Graphics Preset" in the DH config menu

<br>

### **Q: Where can I download the Bukkit plugin?** <br>
A: The bukkit plugin has its own separate download page: <br>
https://modrinth.com/plugin/distant-horizons-support

<br>

# Extended FAQ:

<a href="https://gitlab.com/distant-horizons-team/distant-horizons/-/wikis/1-user-guide/1-frequently-asked-questions/1-general/General">
<img src="https://gitlab.com/jeseibel/minecraft-lod-mod/-/wikis/_hidden/shared-images/images/faq-button-general.png" width="200" />
</a> <br>

<a href="https://gitlab.com/distant-horizons-team/distant-horizons/-/wikis/1-user-guide/1-frequently-asked-questions/4-mod-support/Mod-Support">
<img src="https://gitlab.com/distant-horizons-team/distant-horizons/-/wikis/_hidden/shared-images/images/faq-button-mod-support.png" width="200" />
</a> <br>

<a href="https://gitlab.com/distant-horizons-team/distant-horizons/-/wikis/1-user-guide/1-frequently-asked-questions/2-problems-and-solutions/Problems-and-Solutions">
<img src="https://gitlab.com/distant-horizons-team/distant-horizons/-/wikis/_hidden/shared-images/images/faq-button-problem.png" width="200" />
</a>

<br> <br>

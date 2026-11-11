# Concurrent Chunk Management Engine (Fabric)

A Fabric mod designed to improve the chunk performance of Minecraft.

[Looking for NeoForge versions?](https://modrinth.com/mod/c2me-neoforge)

[Looking for experimental hardware-accelerated world generation as well?](https://modrinth.com/mod/c2me-ocl)

## Benchmarks for 26.2 w/ Terratonic + Terralith (Aug 14th, 2026)
|2t-96t|1t-16t|
|------|------|
|![Performance chart of C2ME, Paper and vanilla from 2 threads to 96 threads. The initial scaling of C2ME stops at roughly 45 threads to ~1200cps due to limited core count, and keeps going up to ~1400cps until the tested 96 threads. Paper doesn't scale linearly and fully stops at roughly 20 threads, scoring at most ~170cps. Vanilla scales slowly up to 42cps. C2ME is up to ~34x perf compared to vanilla, up to ~8.2x perf compared to Paper, on a system with 48 cores.](https://cdn.modrinth.com/data/cached_images/b3e0957bad2d6b9065c721755ca995391283e5ca.png)|![Performance chart of C2ME, Paper and vanilla from 1 threads to 16 threads. C2ME scales fully linearly here, up to 458cps at 16 threads. Paper's linear-ish scaling falls apart starting at 7 threads, and up to 142cps at 16 threads. Vanilla fails to scale linearly at all, up to 41cps at 16 threads. C2ME is roughly 120% faster than vanilla on one threads, scoring 29.1cps and 13.2cps respectively. C2ME is also roughly 130% faster than Paper on average before Paper fails scaling.](https://cdn.modrinth.com/data/cached_images/f66e876c3b13a78572566c2772a16fe7b9ae1237.png)|

## So what is C2ME?
C^2M-Engine, or C2ME for short, is a Fabric mod designed to improve the performance of chunk generation, I/O, and loading. This is done by taking advantage of multiple CPU cores in parallel. For the best performance it is recommended to use C2ME with [Lithium](https://modrinth.com/mod/lithium) and [ScalableLux](https://modrinth.com/mod/scalablelux).

## What does C2ME stand for?
Concurrent chunk management engine, it's about making the game better threaded and more scalable in regard to world gen and chunk io performance.

## Vanilla parity
C2ME does not sacrifice vanilla functionality or behavior, or alter the vanilla world generation in the name of raw speed by default.
However, due to the [non-determinism of vanilla world generation](https://bugs.mojang.com/browse/MC-55596), worlds will vary
significantly run-to-run even with the same seed. This is not a bug on our side. 

While we carefully check that we do not modify any vanilla behavior, bugs are unavoidable after all. 
So, if you do encounter an issue where C2ME deviates from the intended vanilla behavior, don't hesitate to open an issue.

## Mod and Datapack compatibility
World generation datapacks that can run on vanilla Minecraft are fully supported.  
Custom world generators implemented in mods usually runs well, but *may* cause compatibility issues due to certain
design assumption used by mod authors being broken for further speedups of world generation.  
As a world generation mod author, if you find your mod broken, don't hesitate to look for help in our discord server (linked below).
We are willing to help mod authors to embrace scalable world generation.

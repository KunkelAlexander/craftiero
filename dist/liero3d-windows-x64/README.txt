3D SDF Liero (early build)
==========================

A 3D worm shooter with fully destructible terrain. This is a single-player
work-in-progress build: there is no menu and no online multiplayer yet. You
spawn in a cave-and-hills map with an idle practice dummy.

Requirements
------------
* Windows 10 or 11, 64-bit.
* A GPU with a DirectX 12 driver (most GPUs from ~2015 onward).
* Nothing to install. Everything the game needs is inside Liero3D.exe:
  there are no asset files, config files, DLLs or save files.

If launching does nothing, update your graphics driver, or try Vulkan by
opening Command Prompt in this folder and running:
    set WGPU_BACKEND=vulkan
    Liero3D.exe
Other values for WGPU_BACKEND are "gl" and "dx12" (dx12 is the default).

Windows may show "Windows protected your PC" because the exe is not signed.
Click "More info" -> "Run anyway".

How to play
-----------
Double-click Liero3D.exe. A console window shows startup messages; the game
opens its own window. World generation takes a few seconds.

  click the window    capture the mouse
  Esc                 release the mouse; Esc again quits
  W A S D             run (the worm turns); mouse looks and aims
  Space               jump (hold for height), press again in mid-air to
                      double jump
  left mouse          fire (hold)
  right mouse         shoot / release the ninja rope
  1-9 0 - =           pick a weapon (or use the mouse wheel):
                      Blaster, Rocket, Railgun, Bouncer, Bomb, Cluster,
                      Swarm, Teleport, Nova, Block, Tractor, Plasma Whip
  Left Shift          dig (hold): carves a tunnel where you look
  E / Q               reel the rope in (pulls you) / let it out
  W into a wall       climb; S climbs down; Space kicks off the wall
  F                   fly camera + terrain brush (left carve, right build,
                      mouse wheel brush size)
  F1 F2 F3            render at 100% / 75% / 50% resolution
  T                   automatic resolution (default, targets ~60 fps)
  G H O C V           toggle shadows, ambient occlusion, ink outlines,
                      colour grade, vsync
  R                   generate a new map
  M                   mute / unmute sound

Tips
----
* Hold Shift and look slightly down to dig a tunnel you can walk into.
* The Nova charges for about a second before firing.
* The Plasma Whip is melee: hold fire to keep swinging.
* Craters are permanent, and rock is cover from explosions.
* If it runs slowly, press F3 (half resolution) or T (automatic).

License
-------
MIT, see LICENSE. This build includes no third-party assets; all art, sound
and shaders are generated in code.

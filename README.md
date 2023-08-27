# Epitaph - A Wayland Mobile Panel

Epitaph is a Wayland panel focused on providing a touch-friendly interface for
displaying and controlling commonly accessed OS functionality.

## Screenshots

<p align="center">
  <img src="https://user-images.githubusercontent.com/8886672/210189210-6a70de47-1bfe-46e0-b4e7-e4921a9c5ff5.png" width="45%"/>
  <img src="https://user-images.githubusercontent.com/8886672/210189206-3d9d738f-dd60-47bb-99ab-7a6450be9da1.png" width="45%"/>
</p>

## Permissions

To allow toggling Cellular, WiFi, Flashlight, and controlling screen brightness,
it is necessary to grant some Polkit and Udev permissions.

The rules to grant these permissions to users in the `catacomb` group can be
found in the [rules](./rules) directory.

# Epitaph

## Syntax

Epitaph's configuration file uses the TOML format. The format's
specification can be found at _<https://toml.io/en/v1.0.0>_.

## Location

Epitaph doesn't create the configuration file for you, but it looks for one
at <br> `${XDG_CONFIG_HOME:-$HOME/.config}/epitaph/epitaph.toml`.

## Fields

### font

|Name|Description|Type|Default|
|-|-|-|-|
|family|Font family|text|`"sans"`|
|size|Font size|float|`12.0`|

### colors

|Name|Description|Type|Default|
|-|-|-|-|
|background|Background color|color|`"#181818"`|
|module_active||color|`"#555555"`|
|module_inactive|Inactive module background|color|`"#333333"`|
|volume_bg|Volume overlay background|color|`"#752a2a"`|
|volume_bad_bg|Volume overlay background when over 100%|color|`"#ff0000"`|

### input

|Name|Description|Type|Default|
|-|-|-|-|
|max_tap_distance|Square of the maximum distance before touch input is considered a drag|float|`400.0`|
|multi_tap_interval|Maximum time between taps to be considered a double-tap|integer (milliseconds)|`750`|

### geometry

|Name|Description|Type|Default|
|-|-|-|-|
|height|Height of the panel in pixels at scale 1|integer|`20`|
|padding|Panel padding at the screen corners|integer|`5`|
|left|Left-aligned panel modules|["Cellular" | "Battery" | "Clock" | "Wifi" | "Date"]|`[Date]`|
|center|Center-aligned panel modules|["Cellular" | "Battery" | "Clock" | "Wifi" | "Date"]|`[Clock]`|
|right|Right-aligned panel modules|["Cellular" | "Battery" | "Clock" | "Wifi" | "Date"]|`[Cellular, Wifi, Battery]`|

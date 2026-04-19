# yeelib_rs

Fork of https://github.com/teppah/yeelib_rs

## Getting started

Add the following to Cargo.toml:

```toml
yeelib_rs = "0.1.1"
```

Unless otherwise specified, methods to adjust the light's parameters have the method name and behavior exactly as
specified in the above spec.

```rust
use std::time::Duration;
use std::thread::sleep;

use yeelib_rs::{YeeClient, Light, YeeError};
use yeelib_rs::fields::{PowerStatus, Transition};

fn main() -> Result<(), YeeError> {
    let client = YeeClient::new()?;
    let mut lights: Vec<Light> = client.find_lights(Duration::from_secs(1));

    for light in lights.iter_mut() {
        light.set_power(PowerStatus::On, Transition::sudden())?;
        sleep(Duration::from_secs(1));

        light.set_bright(50, Transition::sudden())?;
        sleep(Duration::from_secs(1));

        light.set_ct_abx(3500,
                         Transition::smooth(Duration::from_millis(400))
                             .unwrap())?;
        sleep(Duration::from_secs(2));

        light.toggle()?;
    }
}

```

See [main.rs](src/bin/main.rs) for some more examples.

## Currently supported methods

```
set_ct_abx
set_rgb
set_hsv
set_bright
set_power
toggle
adjust_bright
adjust_ct
bg_set_power
bg_toggle
bg_start_cf
```
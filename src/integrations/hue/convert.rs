use super::bridge::BridgeLight;

use crate::homectl_core::{
    device::{Device, DeviceKind, Light},
    integration::IntegrationId,
    integrations_manager::DeviceId,
};
use palette::{Hsl, IntoColor, Lch};

pub fn to_palette(bridge_light: BridgeLight) -> Option<Lch> {
    let hue: f32 = bridge_light.state.hue? as f32;
    let saturation: f32 = bridge_light.state.sat? as f32;
    let lightness: f32 = bridge_light.state.bri? as f32;

    let hsl = Hsl::new(
        (hue / 65536.0) * 360.0,
        saturation / 254.0,
        lightness / 254.0,
    );
    let lch: Lch = hsl.into_lch();

    Some(lch)
}

pub fn to_light(bridge_light: BridgeLight) -> Light {
    Light {
        power: bridge_light.state.on,
        brightness: None,
        color: to_palette(bridge_light.clone()),
    }
}

pub fn bridge_light_to_device(id: DeviceId, integration_id: IntegrationId, bridge_light: BridgeLight) -> Device {
    let name = bridge_light.name.clone();
    let kind = DeviceKind::Light(to_light(bridge_light));

    Device {
        id,
        name,
        integration_id,
        scene: None,
        kind,
    }
}

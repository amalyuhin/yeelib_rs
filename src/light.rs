use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::net::{SocketAddr, SocketAddrV4, TcpStream};
use std::ops::Add;

use lazy_static::*;
use regex::Regex;
use serde_json::{Value, json};

use crate::err::YeeError;
use crate::fields::{ColorMode, PowerStatus, Rgb};
use crate::req::{Req, Transition};

/// A struct representing an active light on the local network, with interface methods and fields.
#[derive(Debug)]
pub struct Light {
    location: SocketAddrV4,
    id: String,
    model: String,
    fw_ver: u8,
    support: HashSet<String>,
    power: PowerStatus,
    bright: u8,
    color_mode: ColorMode,

    // only valid for ColorMode::ColorTemperature
    ct: u16,

    // only valid for ColorMode::Color
    rgb: Rgb,

    // only valid for ColorMode::Hsv
    hue: u16,
    // only valid for ColorMode::Hsv
    sat: u8,

    name: String,

    // Background light fields (optional - not all devices have them)
    bg_power: Option<PowerStatus>,
    bg_bright: Option<u8>,
    bg_color_mode: Option<ColorMode>,
    bg_ct: Option<u16>,
    bg_rgb: Option<Rgb>,
    bg_hue: Option<u16>,
    bg_sat: Option<u8>,

    // wrapped in option for late init
    // if successfully made a Light, can always assume it is valid
    pub(crate) read: Option<BufReader<TcpStream>>,
    pub(crate) write: Option<BufWriter<TcpStream>>,
}

lazy_static! {
    static ref MATCH_IP: Regex = Regex::new(r#"yeelight://(.*)"#).unwrap();
    static ref MATCH_ERR_MSG: Regex = Regex::new(r#""message":"(.*)""#).unwrap();
}

macro_rules! get_field {
    // for strings
    ($map: expr, $field: expr) => {
        $map.get($field)
            .map(|s| s.as_ref())
            .ok_or(YeeError::FieldNotFound { field_name: stringify!($field) })
    };
    // for primitive types
    ($map: expr, $field: expr, $target_type: ty) => {
        $map.get($field)
            .ok_or(YeeError::FieldNotFound { field_name: stringify!($field) })
            .and_then(|s| {
                let s = s.as_ref();
                s.parse::<$target_type>()
                    .map_err(|e| YeeError::ParseFieldFailed { field_name: stringify!($field), source: Some(e)})
            })
    };
    // for custom FromStr types
    ($map: expr, $field: expr, $target_type: ty, $is_custom_type_marker: expr) => {
        $map.get($field)
            .ok_or(YeeError::FieldNotFound { field_name: stringify!($field) })
            .and_then(|s| {
                let s = s.as_ref();
                s.parse::<$target_type>()
            })
    };
}

macro_rules! check_support {
    ($self: expr, $method: expr) => {
        {
            if !$self.support.contains($method) {
                Err(YeeError::MethodNotSupported { method_name: $method })
            } else {
                Ok(())
            }
        }
    };
}

impl Light {
    pub(crate) fn from_fields<S: AsRef<str>>(fields: &HashMap<&str, S>) -> Result<Light, YeeError> {
        let id = get_field!(fields, "id")?.to_string();
        let model = get_field!(fields, "model")?.to_string();
        let fw_ver = get_field!(fields, "fw_ver", u8)?;
        let power = get_field!(fields, "power", PowerStatus, true)?;
        let support: HashSet<String> = get_field!(fields, "support")?.trim()
            .split_whitespace()
            .map(|s| s.to_string())
            .collect();
        let bright = get_field!(fields, "bright", u8)?;
        let color_mode = get_field!(fields, "color_mode", ColorMode, true)?;
        let ct = get_field!(fields, "ct", u16)?;
        let rgb = get_field!(fields, "rgb", Rgb, true)?;
        let hue: u16 = get_field!(fields, "hue", u16)?;
        let sat = get_field!(fields, "sat", u8)?;
        let name = get_field!(fields, "name")?.to_string();

        let location = get_field!(fields,"Location")?;
        let captures = MATCH_IP
            .captures(location)
            .and_then(|c| c.get(1))
            .ok_or(YeeError::FieldNotFound { field_name: "Location" })
            .and_then(|m| m
                .as_str()
                .parse::<SocketAddr>()
                .map_err(|_| YeeError::ParseFieldFailed { field_name: "Location", source: None })
            )
            ?;
        let location = match captures {
            SocketAddr::V4(v4) => v4,
            _ => panic!("Light should not have an IPv6 address")
        };

        Ok(Light {
            location,
            id,
            model,
            fw_ver,
            power,
            support,
            bright,
            color_mode,
            ct,
            rgb,
            hue,
            sat,
            name,
            bg_power: None,
            bg_bright: None,
            bg_color_mode: None,
            bg_ct: None,
            bg_rgb: None,
            bg_hue: None,
            bg_sat: None,
            read: None,
            write: None
        })
    }

    pub(crate) fn init(&mut self) -> Result<(), YeeError> {
        if self.read.is_some() {
            return Ok(());
        }

        let connection = TcpStream::connect(self.location)?;
        self.write = Some(BufWriter::new(connection.try_clone()?));
        self.read = Some(BufReader::new(connection));

        self.refresh_bg_properties()?;

        Ok(())
    }

    pub fn refresh_bg_properties(&mut self) -> Result<(), YeeError> {
        if !self.support.contains("bg_set_power") {
            return Ok(()); // Device doesn't have background properties
        }

        let props = self.get_prop(&["bg_power", "bg_bright", "bg_lmode", "bg_ct", "bg_rgb", "bg_hue", "bg_sat"])?;

        if props.len() >= 7 {
            self.bg_power = props[0].parse::<PowerStatus>().ok();
            self.bg_bright = props[1].parse::<u8>().ok();
            self.bg_color_mode = props[2].parse::<ColorMode>().ok();
            self.bg_ct = props[3].parse::<u16>().ok();
            self.bg_rgb = props[4].parse::<Rgb>().ok();
            self.bg_hue = props[5].parse::<u16>().ok();
            self.bg_sat = props[6].parse::<u8>().ok();
        }

        Ok(())
    }

    pub fn set_ct_abx(&mut self, temperature: u16, transition: Transition) -> Result<(), YeeError> {
        check_support!(self, "set_ct_abx")?;
        // SPEC IS WRONG: temperature bounds should be 2700-6500
        if !(2700..=6500).contains(&temperature) {
            return Err(YeeError::InvalidValue { field_name: "ct", value: temperature.to_string() });
        }
        let req = Req::new("set_ct_abx".to_string(),
                           vec![json!(temperature), json!(transition.text()), json!(transition.value())]);
        self.send_req(&req)?;
        self.ct = temperature;
        Ok(())
    }

    pub fn set_rgb(&mut self, rgb: Rgb, transition: Transition) -> Result<(), YeeError> {
        check_support!(self, "set_rgb")?;
        let req = Req::new("set_rgb".to_string(),
                           vec![json!(rgb.get_num()), json!(transition.text()), json!(transition.value())]);
        self.send_req(&req)?;
        self.rgb = rgb;
        Ok(())
    }

    pub fn set_bright(&mut self, brightness: u8, transition: Transition) -> Result<(), YeeError> {
        check_support!(self, "set_bright")?;
        if !(1..=100).contains(&brightness) {
            return Err(YeeError::InvalidValue { field_name: "bright", value: brightness.to_string() });
        }
        let req = Req::new("set_bright".to_string(),
                           vec![json!(brightness), json!(transition.text()), json!(transition.value())]);
        self.send_req(&req)?;
        self.bright = brightness;
        Ok(())
    }

    pub fn set_hsv(&mut self, hue: u16, sat: u8, transition: Transition) -> Result<(), YeeError> {
        check_support!(self, "set_hsv")?;
        if !(0..=359).contains(&hue) {
            return Err(YeeError::InvalidValue { field_name: "hue", value: hue.to_string() });
        } else if !(0..=100).contains(&sat) {
            return Err(YeeError::InvalidValue { field_name: "sat", value: sat.to_string() });
        }
        let req = Req::new("set_hsv".to_string(),
                           vec![json!(hue), json!(sat), json!(transition.text()), json!(transition.value())]);
        self.send_req(&req)?;
        self.hue = hue;
        self.sat = sat;
        Ok(())
    }

    pub fn set_power(&mut self, power: PowerStatus, transition: Transition) -> Result<(), YeeError> {
        check_support!(self, "set_power")?;
        let req = Req::new("set_power".to_string(),
                           vec![json!(power.to_string()), json!(transition.text()), json!(transition.value())]);
        self.send_req(&req)?;
        self.power = power;
        Ok(())
    }

    pub fn toggle(&mut self) -> Result<(), YeeError> {
        check_support!(self, "toggle")?;
        let req = Req::new("toggle".to_string(), vec![]);
        self.send_req(&req)?;
        self.power = self.power.flip();
        Ok(())
    }

    pub fn adjust_bright(&mut self, bright_percentage: i8, transition: Transition) -> Result<(), YeeError> {
        if !(-100 as i8..=100).contains(&bright_percentage) {
            return Err(YeeError::InvalidValue { field_name: "bright", value: bright_percentage.to_string() });
        }
        check_support!(self, "adjust_bright")?;
        let req = Req::new("adjust_bright".to_string(), vec![json!(bright_percentage), json!(transition.value())]);
        self.send_req(&req)?;
        if bright_percentage > 0 {
            self.bright += self.bright * bright_percentage as u8 / 100;
        } else {
            self.bright -= self.bright * bright_percentage.abs() as u8 / 100;
        }
        Ok(())
    }

    pub fn adjust_ct(&mut self, ct_percentage: i8, transition: Transition) -> Result<(), YeeError> {
        check_support!(self, "adjust_ct")?;
        if !(-100 as i8..=100).contains(&ct_percentage) {
            return Err(YeeError::InvalidValue { field_name: "ct", value: ct_percentage.to_string() });
        }
        check_support!(self, "adjust_ct")?;
        let req = Req::new("adjust_ct".to_string(), vec![json!(ct_percentage), json!(transition.value())]);
        self.send_req(&req)?;
        if ct_percentage > 0 {
            self.ct += self.ct * ct_percentage as u16 / 100;
        } else {
            self.ct -= self.ct * ct_percentage.abs() as u16
        }
        Ok(())
    }

    pub fn bg_set_power(&mut self, power: PowerStatus, transition: Transition) -> Result<(), YeeError> {
        check_support!(self, "bg_set_power")?;
        let req = Req::new("bg_set_power".to_string(), vec![json!(power.to_string()), json!(transition.text()), json!(transition.text())]);
        self.send_req(&req)?;
        self.bg_power = Some(power);
        Ok(())
    }

    pub fn bg_toggle(&mut self) -> Result<(), YeeError> {
        check_support!(self, "bg_toggle")?;
        let req = Req::new("bg_toggle".to_string(), vec![]);
        self.send_req(&req)?;

        if self.bg_power.is_some() {
            self.bg_power = Some(self.bg_power.unwrap().flip());
        }

        Ok(())
    }

    pub fn bg_start_cf(&mut self, count: u8, action: u8, flow: Vec<(u32, u8, u32, u8)>) -> Result<(), YeeError> {
        check_support!(self, "bg_start_cf")?;

        let flow_expression = flow.into_iter()
            .map(|(d, m, v, b)| format!("{d}, {m}, {v}, {b}"))
            .collect::<Vec<String>>()
            .join(",");

        let req = Req::new("bg_start_cf".to_string(), vec![json!(count), json!(action), json!(flow_expression)]);
        self.send_req(&req)?;

        Ok(())
    }

    pub(crate) fn send_req(&mut self, req: &Req) -> Result<(), YeeError> {
        let rand_val = req.id.to_string();
        let mut json = serde_json::to_string(req).unwrap();
        let reader = self.read.as_mut().unwrap();
        let writer = self.write.as_mut().unwrap();
        json.push_str("\r\n");
        writer.write_all(json.as_bytes())?;
        writer.flush()?;

        let mut buf = String::new();
        let rand_val = rand_val.to_string();
        while !buf.contains(rand_val.as_str()) {
            reader.read_line(&mut buf)?;
        }
        if buf.contains("error") {
            let s =
                MATCH_ERR_MSG.captures(&buf)
                    .and_then(|c| c.get(0))
                    .map(|s| s.as_str().to_string())
                    .unwrap_or_else(String::new);
            Err(YeeError::ChangeFailed { message: s })
        } else {
            Ok(())
        }
    }

    pub fn get_prop(&mut self, props: &[&str]) -> Result<Vec<String>, YeeError> {
        check_support!(self, "get_prop")?;
        let params: Vec<Value> = props.iter().map(|&p| json!(p)).collect();
        let req = Req::new("get_prop".to_string(), params);

        let rand_val = req.id.to_string();
        let mut json = serde_json::to_string(&req).unwrap();
        let reader = self.read.as_mut().unwrap();
        let writer = self.write.as_mut().unwrap();
        json.push_str("\r\n");
        writer.write_all(json.as_bytes())?;
        writer.flush()?;

        let mut buf = String::new();
        while !buf.contains(&rand_val) {
            reader.read_line(&mut buf)?;
        }

        if buf.contains("error") {
            let s = MATCH_ERR_MSG.captures(&buf)
                .and_then(|c| c.get(0))
                .map(|s| s.as_str().to_string())
                .unwrap_or_else(String::new);
            Err(YeeError::ChangeFailed { message: s })
        } else {
            let response: serde_json::Value = serde_json::from_str(&buf)
                .map_err(|_| YeeError::ChangeFailed { message: "Failed to parse response".to_string() })?;
            let results = response["result"].as_array()
                .ok_or(YeeError::ChangeFailed { message: "No result array".to_string() })?;

            Ok(results.iter().map(|v| v.as_str().unwrap_or("").to_string()).collect())
        }
    }

    pub fn location(&self) -> &SocketAddrV4 {
        &self.location
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn fw_ver(&self) -> u8 {
        self.fw_ver
    }

    pub fn support(&self) -> &HashSet<String> {
        &self.support
    }

    pub fn power(&self) -> &PowerStatus {
        &self.power
    }

    pub fn bright(&self) -> u8 {
        self.bright
    }

    pub fn color_mode(&self) -> &ColorMode {
        &self.color_mode
    }

    pub fn ct(&self) -> u16 {
        self.ct
    }

    pub fn rgb(&self) -> &Rgb {
        &self.rgb
    }

    pub fn hue(&self) -> u16 {
        self.hue
    }

    pub fn sat(&self) -> u8 {
        self.sat
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

impl Hash for Light {
    fn hash<H: Hasher>(&self, state: &mut H) {
        state.write(self.id.as_bytes());
    }
}

impl PartialEq for Light {
    fn eq(&self, other: &Self) -> bool {
        self.id.eq(&other.id)
    }
}

impl Eq for Light {}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::net::{IpAddr, Ipv4Addr, SocketAddrV4, TcpListener};

    use super::*;

    macro_rules! map {
        ($($key:expr => $value: expr), *) => {{
            let mut map = HashMap::new();
            $(map.insert($key,$value);)*
            map
        }};
    }

    pub(crate) fn get_map() -> HashMap<&'static str, &'static str> {
        let mut m: HashMap<&str, &str> =
            map!(
            "id" => "0x1234",
            "model" => "floor",
            "fw_ver" => "40", // can fail
            "power" => "on", // can fail
            "bright" => "34", // can fail
            "color_mode" => "2", // can fail
            "ct" => "0", // can fail
            "rgb" => "657930", // 0A0A0A, can fail
            "hue" => "314", // can fail
            "sat" => "12", // can fail
            "name" => "room_light",
            "Location" => "yeelight://127.0.0.1:13454"
            );
        let support = "get_power set_power get_rgb set_rgb";
        m.insert("support", support);
        m
    }

    #[test]
    fn send_correct_req() -> anyhow::Result<()> {
        Ok(())
    }

    #[test]
    fn get_correct_location() -> anyhow::Result<()> {
        // given
        let map = get_map();
        let expected_addr = match SocketAddr::new(IpAddr::from(Ipv4Addr::LOCALHOST), 13454) {
            SocketAddr::V4(v4) => v4,
            _ => unreachable!()
        };

        // when
        let light = Light::from_fields(&map)?;

        // then
        assert_eq!(*light.location(), expected_addr);
        Ok(())
    }

    macro_rules! generate_getter_tests {
        () => {};
        ($field:ident, $($tail: tt)*) => {
            #[test]
            fn $field() -> anyhow::Result<()> {

                // given
                let map = get_map();

                // when
                let light = Light::from_fields(&map)?;

                // then
                assert_eq!(map.get(stringify!($field)).unwrap(), &light.$field().to_string());
                Ok(())
            }
            generate_getter_tests!($($tail)*);
        };
        ($field:ident => $expected: expr, $($tail: tt)*) => {
            #[test]
            fn $field() -> anyhow::Result<()> {

                // given
                let map = get_map();

                // when
                let light = Light::from_fields(&map)?;

                // then
                assert_eq!(&$expected, light.$field());
                Ok(())
            }
            generate_getter_tests!($($tail)*);
        };

    }

    mod test_get_parse {
        use super::*;

        generate_getter_tests!(
            id,
            model,
            fw_ver,
            power,
            bright,
            color_mode => ColorMode::ColorTemperature,
            ct,
            rgb => Rgb { red: 10, green: 10, blue: 10 },
            hue,
            sat,
            name, );
    }

    macro_rules! generate_parse_fail_tests {
        ($($field:ident), *) => {
            $(
                #[test]
                fn $field() {

                    // given
                    let mut map = get_map();
                    map.remove(stringify!($field)).unwrap();

                    // when
                    let fail = Light::from_fields(&map);

                    // then
                    assert!(fail.is_err());
                }
            )*
        };
    }

    mod test_parse_fail {
        use super::*;

        generate_parse_fail_tests!(
            id,
            model,
            fw_ver,
            support,
            power,
            bright,
            color_mode,
            ct,
            rgb,
            hue,
            sat,
            name);
    }

    #[test]
    fn get_correct_support() -> anyhow::Result<()> {
        // given
        let map = get_map();
        let expected_fields: HashSet<String> = map.get("support").unwrap().split_whitespace().map(|s| s.to_string()).collect();

        // when
        let light = Light::from_fields(&map)?;

        // then
        let support = light.support();
        assert_eq!(&expected_fields, support);
        Ok(())
    }

    #[test]
    fn correctly_connects() -> anyhow::Result<()> {
        // given
        let map = get_map();
        let addr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 13454);
        let _fake_listener = TcpListener::bind(addr)?;

        // when
        let mut light = Light::from_fields(&map)?;
        light.init()?;

        // then
        assert!(light.read.is_some());
        assert!(light.write.is_some());
        Ok(())
    }
}

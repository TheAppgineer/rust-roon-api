use std::sync::Arc;
use serde::{Deserialize, Serialize};
use serde_json::{Error, json};
use tokio::sync::Mutex;

use crate::{Moo, Parsed};

pub const SVCNAME: &str = "com.roonlabs.transport:2";

pub mod volume {
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
    #[serde(rename_all = "snake_case")]
    pub enum Scale {
        Number,
        #[serde(rename = "db")] Decibel,
        Incremental,
    }

    #[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
    #[serde(rename_all = "snake_case")]
    pub enum ChangeMode {
        Absolute,
        Relative,
        RelativeStep,
    }

    #[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
    #[serde(rename_all = "snake_case")]
    pub enum Mute {
        Mute,
        Unmute,
    }

    #[derive(Clone, Debug, Deserialize)]
    pub struct Volume {
        #[serde(rename = "type")] pub scale: Scale,
        pub min: Option<i8>,
        pub max: Option<i8>,
        pub value: Option<i8>,
        pub step: Option<i8>,
        pub is_muted: Option<bool>,
        pub hard_limit_min: i8,
        pub hard_limit_max: i8,
        pub soft_limit: i8,
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Playing,
    Paused,
    Loading,
    Stopped,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Control {
    Play,
    Pause,
    PlayPause,
    Stop,
    Previous,
    Next,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum Repeat {
    #[serde(rename = "disabled")] Off,
    #[serde(rename = "loop")]     All,
    #[serde(rename = "loop_one")] One,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Selected,
    Deselected,
    Standby,
    Indeterminate,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Seek {
    Absolute,
    Relative,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Zone {
    pub zone_id: String,
    pub display_name: String,
    pub outputs: Vec<Output>,
    pub state: State,
    pub is_next_allowed: bool,
    pub is_previous_allowed: bool,
    pub is_pause_allowed: bool,
    pub is_play_allowed: bool,
    pub is_seek_allowed: bool,
    pub queue_items_remaining: i64,
    pub queue_time_remaining: i64,
    pub now_playing: Option<NowPlaying>,
    pub settings: Settings,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ZoneSeek {
    pub zone_id: String,
    pub queue_time_remaining: i64,
    pub seek_position: Option<i64>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Output {
    pub output_id: String,
    pub zone_id: String,
    pub can_group_with_output_ids: Vec<String>,
    pub display_name: String,
    pub volume: Option<volume::Volume>,
    pub source_controls: Vec<SourceControls>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct SourceControls {
    pub control_key: String,
    pub display_name: String,
    pub supports_standby: bool,
    pub status: Status,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Settings {
    #[serde(rename = "loop")] pub repeat: Repeat,
    pub shuffle: bool,
    pub auto_radio: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct NowPlaying {
    pub artist_image_keys: Option<Vec<String>>,
    pub image_key: Option<String>,
    pub length: Option<u32>,
    pub seek_position: Option<i64>,
    pub one_line: OneLine,
    pub two_line: TwoLine,
    pub three_line: ThreeLine,
}

#[derive(Clone, Debug, Deserialize)]
pub struct QueueItem {
    pub image_key: Option<String>,
    pub length: u32,
    pub queue_item_id: u32,
    pub one_line: OneLine,
    pub two_line: TwoLine,
    pub three_line: ThreeLine,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QueueOperation {
    Insert,
    Remove,
}

#[derive(Clone, Debug, Deserialize)]
pub struct QueueChange {
    pub operation: QueueOperation,
    pub index: usize,
    pub items: Option<Vec<QueueItem>>,
    pub count: Option<usize>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct OneLine {
    pub line1: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct TwoLine {
    pub line1: String,
    pub line2: String,
}

#[derive(Clone, Debug, Deserialize)]
pub struct ThreeLine {
    pub line1: String,
    pub line2: String,
    pub line3: String,
}

#[derive(Clone, Debug)]
pub struct Transport {
    moo: Option<Moo>,
    zone_sub: Arc<Mutex<Option<(usize, usize)>>>,
    output_sub: Arc<Mutex<Option<(usize, usize)>>>,
    queue_sub: Arc<Mutex<Option<(usize, usize)>>>,
    zone_req_id: Arc<Mutex<Option<usize>>>,
    output_req_id: Arc<Mutex<Option<usize>>>,
}

impl Transport {
    pub fn new() -> Self {
        Self {
            moo: None,
            zone_sub: Arc::new(Mutex::new(None)),
            output_sub: Arc::new(Mutex::new(None)),
            queue_sub: Arc::new(Mutex::new(None)),
            zone_req_id: Arc::new(Mutex::new(None)),
            output_req_id: Arc::new(Mutex::new(None)),
        }
    }

    pub fn set_moo(&mut self, moo: Moo) {
        self.moo = Some(moo);
    }

    pub async fn mute_all(&self, how: &volume::Mute) -> Option<usize> {
        let moo = self.moo.as_ref()?;
        let how = how.serialize(serde_json::value::Serializer).ok()?;
        let body = json!({
            "how": how,
        });

        moo.send_req(SVCNAME.to_owned() + "/mute_all", Some(body)).await.ok()
    }

    pub async fn pause_all(&self) -> Option<usize> {
        self.moo.as_ref()?.send_req(SVCNAME.to_owned() + "/pause_all", None).await.ok()
    }

    pub async fn standby(&self, output_id: &str, control_key: Option<&str>) -> Option<usize> {
        let moo = self.moo.as_ref()?;
        let mut body = json!({
            "output_id": output_id,
        });

        if let Some(control_key) = control_key {
            body["control_key"] = control_key.into();
        }

        moo.send_req(SVCNAME.to_owned() + "/standby", Some(body)).await.ok()
    }

    pub async fn toggle_standby(&self, output_id: &str, control_key: Option<&str>) -> Option<usize> {
        let moo = self.moo.as_ref()?;
        let mut body = json!({
            "output_id": output_id,
        });

        if let Some(control_key) = control_key {
            body["control_key"] = control_key.into();
        }

        moo.send_req(SVCNAME.to_owned() + "/toggle_standby", Some(body)).await.ok()
    }

    pub async fn convenience_switch(&self, output_id: &str, control_key: Option<&str>) -> Option<usize> {
        let moo = self.moo.as_ref()?;
        let mut body = json!({
            "output_id": output_id,
        });

        if let Some(control_key) = control_key {
            body["control_key"] = control_key.into();
        }

        moo.send_req(SVCNAME.to_owned() + "/convenience_switch", Some(body)).await.ok()
    }

    pub async fn mute(&self, output_id: &str, how: &volume::Mute) -> Option<usize> {
        let moo = self.moo.as_ref()?;
        let how = how.serialize(serde_json::value::Serializer).ok()?;
        let body = json!({
            "output_id": output_id,
            "how": how,
        });

        moo.send_req(SVCNAME.to_owned() + "/mute", Some(body)).await.ok()
    }

    pub async fn change_volume(&self, output_id: &str, how: &volume::ChangeMode, value: i32) -> Option<usize> {
        let moo = self.moo.as_ref()?;
        let how = how.serialize(serde_json::value::Serializer).ok()?;
        let body = json!({
            "output_id": output_id,
            "how": how,
            "value": value,
        });

        moo.send_req(SVCNAME.to_owned() + "/change_volume", Some(body)).await.ok()
    }

    pub async fn seek(&self, zone_or_output_id: &str, how: &Seek, seconds: i32) -> Option<usize> {
        let moo = self.moo.as_ref()?;
        let how = how.serialize(serde_json::value::Serializer).ok()?;
        let body = json!({
            "zone_or_output_id": zone_or_output_id,
            "how": how,
            "seconds": seconds,
        });

        moo.send_req(SVCNAME.to_owned() + "/seek", Some(body)).await.ok()
    }

    pub async fn control(&self, zone_or_output_id: &str, control: &Control) -> Option<usize> {
        let moo = self.moo.as_ref()?;
        let control = control.serialize(serde_json::value::Serializer).ok()?;
        let body = json!({
            "zone_or_output_id": zone_or_output_id,
            "control": control,
        });

        moo.send_req(SVCNAME.to_owned() + "/control", Some(body)).await.ok()
    }

    pub async fn transfer_zone(&self, from_zone_or_output_id: &str, to_zone_or_output_id: &str) -> Option<usize> {
        let moo = self.moo.as_ref()?;
        let body = json!({
            "from_zone_or_output_id": from_zone_or_output_id,
            "to_zone_or_output_id": to_zone_or_output_id,
        });

        moo.send_req(SVCNAME.to_owned() + "/transfer_zone", Some(body)).await.ok()
    }

    pub async fn group_outputs(&self, output_ids: Vec<&str>) -> Option<usize> {
        let moo = self.moo.as_ref()?;
        let body = json!({
            "output_ids": output_ids,
        });

        moo.send_req(SVCNAME.to_owned() + "/group_outputs", Some(body)).await.ok()
    }

    pub async fn ungroup_outputs(&self, output_ids: Vec<&str>) -> Option<usize> {
        let moo = self.moo.as_ref()?;
        let body = json!({
            "output_ids": output_ids,
        });

        moo.send_req(SVCNAME.to_owned() + "/ungroup_outputs", Some(body)).await.ok()
    }

    pub async fn change_settings(&self, zone_or_output_id: &str, settings: Settings) -> Option<usize> {
        let moo = self.moo.as_ref()?;
        let mut body = settings.serialize(serde_json::value::Serializer).ok()?;

        body["zone_or_output_id"] = zone_or_output_id.into();

        moo.send_req(SVCNAME.to_owned() + "/change_settings", Some(body)).await.ok()
    }

    pub async fn get_zones(&self) {
        if let Some(moo) = &self.moo {
            let zone_req_id = moo.send_req(SVCNAME.to_owned() + "/get_zones", None).await.ok();

            *self.zone_req_id.lock().await = zone_req_id;
        }
    }

    pub async fn get_outputs(&self) {
        if let Some(moo) = &self.moo {
            let output_req_id = moo.send_req(SVCNAME.to_owned() + "/get_outputs", None).await.ok();

            *self.output_req_id.lock().await = output_req_id;
        }
    }

    pub async fn subscribe_zones(&self) {
        if let Some(moo) = &self.moo {
            let sub = moo.send_sub_req(SVCNAME, "zones", None).await.ok();

            *self.zone_sub.lock().await = sub;
        }
    }

    pub async fn unsubscribe_zones(&self) {
        if let Some(moo) = &self.moo {
            let mut sub = self.zone_sub.lock().await;

            if let Some((_, sub_key)) = *sub {
                moo.send_unsub_req(SVCNAME, "zones", sub_key).await.ok();

                *sub = None;
            }
        }
    }

    pub async fn subscribe_outputs(&self) {
        if let Some(moo) = &self.moo {
            let sub = moo.send_sub_req(SVCNAME, "outputs", None).await.ok();

            *self.output_sub.lock().await = sub;
        }
    }

    pub async fn unsubscribe_outputs(&self) {
        if let Some(moo) = &self.moo {
            let mut sub = self.output_sub.lock().await;

            if let Some((_, sub_key)) = *sub {
                moo.send_unsub_req(SVCNAME, "outputs", sub_key).await.ok();

                *sub = None;
            }
        }
    }

    pub async fn subscribe_queue(&self, zone_or_output_id: &str, max_item_count: u32) {
        if let Some(moo) = &self.moo {
            let args = json!({
                "zone_or_output_id": zone_or_output_id,
                "max_item_count": max_item_count,
            });
            let sub = moo.send_sub_req(SVCNAME, "queue", Some(args)).await.ok();

            *self.queue_sub.lock().await = sub;
        }
    }

    pub async fn unsubscribe_queue(&self) {
        if let Some(moo) = &self.moo {
            let mut sub = self.queue_sub.lock().await;

            if let Some((_, sub_key)) = *sub {
                moo.send_unsub_req(SVCNAME, "queue", sub_key).await.ok();

                *sub = None;
            }
        }
    }

    pub async fn play_from_here(&self, zone_or_output_id: &str, queue_item_id: u32) -> Option<usize> {
        let moo = self.moo.as_ref()?;
        let body = json!({
            "zone_or_output_id": zone_or_output_id,
            "queue_item_id": queue_item_id,
        });

        moo.send_req(SVCNAME.to_owned() + "/play_from_here", Some(body)).await.ok()
    }

    pub async fn parse_msg(&self, msg: &serde_json::Value) -> Result<Vec<Parsed>, Error> {
        let req_id = msg["request_id"].as_str().unwrap().parse::<usize>().unwrap();
        let response = msg["name"].as_str().unwrap();
        let body = msg["body"].to_owned();
        let mut parsed = Vec::new();

        if let Some((zone_req_id, _)) = *self.zone_sub.lock().await {
            if req_id == zone_req_id {
                if response == "Changed" {
                    if body["zones_changed"].is_array() {
                        let zones = serde_json::from_value(body["zones_changed"].to_owned())?;
                        parsed.push(Parsed::Zones(zones));
                    }

                    if body["zones_added"].is_array() {
                        let zones = serde_json::from_value(body["zones_added"].to_owned())?;
                        parsed.push(Parsed::Zones(zones));
                    }

                    if body["zones_seek_changed"].is_array() {
                        let zones_seek = serde_json::from_value(body["zones_seek_changed"].to_owned())?;
                        parsed.push(Parsed::ZonesSeek(zones_seek));
                    }

                    if body["zones_removed"].is_array() {
                        let zones_removed = serde_json::from_value(body["zones_removed"].to_owned())?;
                        parsed.push(Parsed::ZonesRemoved(zones_removed));
                    }
                } else if response == "Subscribed" {
                    if body["zones"].is_array() {
                        let zones = serde_json::from_value(body["zones"].to_owned())?;
                        parsed.push(Parsed::Zones(zones));
                    }
                }
            }
        }

        if let Some(zone_req_id) = *self.zone_req_id.lock().await {
            if req_id == zone_req_id && response == "Success" {
                if body["zones"].is_array() {
                    let zones = serde_json::from_value(body["zones"].to_owned())?;
                    parsed.push(Parsed::Zones(zones));
                }
            }
        }

        if let Some(output_req_id) = *self.output_req_id.lock().await {
            if req_id == output_req_id && response == "Success" {
                if body["outputs"].is_array() {
                    let outputs = serde_json::from_value(body["outputs"].to_owned())?;
                    parsed.push(Parsed::Outputs(outputs));
                }
            }
        }

        if let Some((output_req_id, _)) = *self.output_sub.lock().await {
            if req_id == output_req_id {
                if response == "Changed" {
                    if body["outputs_changed"].is_array() {
                        let outputs = serde_json::from_value(body["outputs_changed"].to_owned())?;
                        parsed.push(Parsed::Outputs(outputs));
                    }

                    if body["outputs_added"].is_array() {
                        let outputs = serde_json::from_value(body["outputs_added"].to_owned())?;
                        parsed.push(Parsed::Outputs(outputs));
                    }

                    if body["outputs_removed"].is_array() {
                        let outputs_removed = serde_json::from_value(body["outputs_removed"].to_owned())?;
                        parsed.push(Parsed::OutputsRemoved(outputs_removed));
                    }
                } else if response == "Subscribed" {
                    if body["outputs"].is_array() {
                        let outputs = serde_json::from_value(body["outputs"].to_owned())?;
                        parsed.push(Parsed::Outputs(outputs));
                    }
                }
            }
        }

        if let Some((queue_req_id, _)) = *self.queue_sub.lock().await {
            if req_id == queue_req_id {
                if response == "Changed" {
                    if body["changes"].is_array() {
                        let changes = serde_json::from_value(body["changes"].to_owned())?;

                        parsed.push(Parsed::QueueChanges(changes));
                    }
                } else if response == "Subscribed" {
                    if body["items"].is_array() {
                        let queue = serde_json::from_value(body["items"].to_owned())?;

                        parsed.push(Parsed::Queue(queue));
                    }
                }
            }
        }

        return Ok(parsed)
    }
}

#[cfg(test)]
#[cfg(feature = "transport")]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::{RoonApi, CoreEvent, Info, Svc, Services, info};

    #[tokio::test(flavor = "current_thread")]
    async fn it_works() {
        const CONFIG_PATH: &str = "config.json";
        let info = info!("com.theappgineer", "Rust Roon API");
        let mut roon = RoonApi::new(info);
        let services = vec![Services::Transport(Transport::new())];
        let provided: HashMap<String, Svc> = HashMap::new();
        fn get_roon_state() -> serde_json::Value {
            RoonApi::load_config(CONFIG_PATH, "roonstate")
        }
        let (mut handles, mut core_rx) = roon.start_discovery(get_roon_state, provided, Some(services)).await.unwrap();

        handles.push(tokio::spawn(async move {
            let mut transport = None;

            loop {
                if let Some((core, msg)) = core_rx.recv().await {
                    match core {
                        CoreEvent::Found(mut core) => {
                            println!("Core found: {}, version {}", core.display_name, core.display_version);

                            transport = core.get_transport().cloned();

                            if let Some(transport) = transport.as_ref() {
                                transport.subscribe_zones().await;
                                transport.subscribe_outputs().await;
                            }
                        }
                        CoreEvent::Lost(core) => {
                            println!("Core lost: {}, version {}", core.display_name, core.display_version);
                        }
                        _ => ()
                    }

                    if let Some((msg, parsed)) = msg {
                        match parsed {
                            Parsed::RoonState => {
                                RoonApi::save_config(CONFIG_PATH, "roonstate", msg).unwrap();
                            }
                            Parsed::Zones(zones) => {
                                for zone in zones {
                                    if zone.settings.auto_radio {
                                        if let Some(transport) = transport.as_ref() {
                                            let mut settings = zone.settings;

                                            settings.auto_radio = false;
                                            transport.change_settings(&zone.zone_id, settings).await;
                                        }
                                    }
                                }
                            }
                            Parsed::ZonesSeek(_zones_seek) => (),
                            Parsed::ZonesRemoved(_zones_removed) => {
                                if let Some(transport) = transport.as_ref() {
                                    transport.unsubscribe_zones().await;
                                }
                            }
                            Parsed::Outputs(outputs) => {
                                let output_id = &outputs[0].output_id;

                                if let Some(transport) = transport.as_ref() {
                                    transport.subscribe_queue(&output_id, 20).await;
                                }
                            }
                            Parsed::OutputsRemoved(_outputs_removed) => (),
                            Parsed::Queue(_queue) => (),
                            _ => ()
                        }
                    }
                }
            }
        }));

        for handle in handles {
            handle.await.unwrap();
        }
    }
}

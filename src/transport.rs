use std::sync::{Arc, Mutex};
use serde::Deserialize;
use serde_json::json;

use crate::{Moo, Parsed};

pub const SVCNAME: &str = "com.roonlabs.transport:2";

#[derive(Debug, Deserialize)]
pub struct Zone {
    pub zone_id: String,
    pub display_name: String,
    pub outputs: Vec<Output>,
    pub state: String,
    pub is_next_allowed: bool,
    pub is_previous_allowed: bool,
    pub is_pause_allowed: bool,
    pub is_play_allowed: bool,
    pub is_seek_allowed: bool,
    pub queue_items_remaining: i64,
    pub queue_time_remaining: i64,
    pub now_playing: Option<NowPlaying>,
    pub settings: Settings
}

#[derive(Debug, Deserialize)]
pub struct ZoneSeek {
    pub zone_id: String,
    pub queue_time_remaining: i64,
    pub seek_position: i64
}

#[derive(Debug, Deserialize)]
pub struct Output {
    pub output_id: String,
    pub zone_id: String,
    pub can_group_with_output_ids: Vec<String>,
    pub display_name: String,
    pub volume: Volume,
    pub source_controls: Vec<SourceControls>
}

#[derive(Debug, Deserialize)]
pub struct Volume {
    #[serde(rename = "type")] pub scale: String,
    pub min: i8,
    pub max: i8,
    pub value: i8,
    pub step: i8,
    pub is_muted: bool,
    pub hard_limit_min: i8,
    pub hard_limit_max: i8,
    pub soft_limit: i8
}

#[derive(Debug, Deserialize)]
pub struct SourceControls {
    pub control_key: String,
    pub display_name: String,
    pub supports_standby: bool,
    pub status: String
}

#[derive(Debug, Deserialize)]
pub struct Settings {
    #[serde(rename = "loop")] pub repeat: String,
    pub shuffle: bool,
    pub auto_radio: bool
}

#[derive(Debug, Deserialize)]
pub struct NowPlaying {
    pub image_key: String,
    pub seek_position: Option<i64>,
    pub one_line: OneLine,
    pub two_line: TwoLine,
    pub three_line: ThreeLine
}

#[derive(Debug, Deserialize)]
pub struct QueueItem {
    pub image_key: String,
    pub length: u32,
    pub queue_item_id: u32,
    pub one_line: OneLine,
    pub two_line: TwoLine,
    pub three_line: ThreeLine
}

#[derive(Debug, Deserialize)]
pub struct OneLine {
    pub line1: String
}

#[derive(Debug, Deserialize)]
pub struct TwoLine {
    pub line1: String,
    pub line2: String
}

#[derive(Debug, Deserialize)]
pub struct ThreeLine {
    pub line1: String,
    pub line2: String,
    pub line3: String
}

#[derive(Clone, Debug)]
pub struct Transport {
    moo: Option<Moo>,
    zone_req_id: Arc<Mutex<Option<usize>>>,
    output_req_id: Arc<Mutex<Option<usize>>>,
    queue_req_id: Arc<Mutex<Option<usize>>>
}

impl Transport {
    pub fn new() -> Self {
        Self {
            moo: None,
            zone_req_id: Arc::new(Mutex::new(None)),
            output_req_id: Arc::new(Mutex::new(None)),
            queue_req_id: Arc::new(Mutex::new(None))
        }
    }

    pub fn set_moo(&mut self, moo: Moo) {
        self.moo = Some(moo);
    }

    pub async fn get_zones(&self) -> Option<usize> {
        if let Some(moo) = &self.moo {
            moo.send_req(SVCNAME.to_owned() + "/get_zones", None).await.ok()
        } else {
            None
        }
    }

    pub async fn subscribe_zones(&mut self) {
        if let Some(moo) = &self.moo {
            let req_id = moo.send_sub_req(SVCNAME, "zones", None).await.ok();
            let mut req_id_option = self.zone_req_id.lock().unwrap();

            *req_id_option = req_id;
        }
    }

    pub async fn subscribe_outputs(&mut self) {
        if let Some(moo) = &self.moo {
            let req_id = moo.send_sub_req(SVCNAME, "outputs", None).await.ok();
            let mut req_id_option = self.output_req_id.lock().unwrap();

            *req_id_option = req_id;
        }
    }

    pub async fn subscribe_queue(&mut self, zone_or_output_id: &str, max_item_count: u32) {
        if let Some(moo) = &self.moo {
            let args = json!({
                "zone_or_output_id": zone_or_output_id,
                "max_item_count": max_item_count
            });
            let req_id = moo.send_sub_req(SVCNAME, "queue", Some(args)).await.ok();
            let mut req_id_option = self.queue_req_id.lock().unwrap();

            *req_id_option = req_id;
        }
    }

    pub async fn control(&self, zone_or_output_id: &str, control: &str) -> Option<usize> {
        if let Some(moo) = &self.moo {
            let body = json!({
                "zone_or_output_id": zone_or_output_id,
                "control": control
            });

            moo.send_req(SVCNAME.to_owned() + "/control", Some(body)).await.ok()
        } else {
            None
        }
    }

    pub fn parse_msg(&self, msg: &serde_json::Value) -> Parsed {
        let req_id = msg["request_id"].as_str().unwrap().parse::<usize>().unwrap();
        let response = msg["name"].as_str().unwrap();
        let body = msg["body"].to_owned();

        if let Some(zone_req_id) = *self.zone_req_id.lock().unwrap() {
            if req_id == zone_req_id {
                let mut zones: Option<Vec<Zone>> = None;

                if response == "Changed" {
                    if body["zones_changed"].is_array() {
                        zones = serde_json::from_value(body["zones_changed"].to_owned()).ok();
                    } else if body["zones_seek_changed"].is_array() {
                        let zones_seek: Option<Vec<ZoneSeek>> = serde_json::from_value(body["zones_seek_changed"].to_owned()).ok();

                        return match zones_seek {
                            Some(zones_seek) => Parsed::ZonesSeek(zones_seek),
                            None => Parsed::None
                        }
                    }
                } else if response == "Subscribed" {
                    if body["zones"].is_array() {
                        zones = serde_json::from_value(body["zones"].to_owned()).ok();
                    }
                }

                return match zones {
                    Some(zones) => Parsed::Zones(zones),
                    None => Parsed::None
                }
            }
        }

        if let Some(output_req_id) = *self.output_req_id.lock().unwrap() {
            if req_id == output_req_id {
                let mut outputs: Option<Vec<Output>> = None;

                if response == "Changed" {
                    if body["outputs_changed"].is_array() {
                        outputs = serde_json::from_value(body["outputs_changed"].to_owned()).ok();
                    }
                } else if response == "Subscribed" {
                    if body["outputs"].is_array() {
                        outputs = serde_json::from_value(body["outputs"].to_owned()).ok();
                    }
                }

                return match outputs {
                    Some(outputs) => Parsed::Outputs(outputs),
                    None => Parsed::None
                }
            }
        }

        if let Some(queue_req_id) = *self.queue_req_id.lock().unwrap() {
            if req_id == queue_req_id {
                let mut queue: Option<Vec<QueueItem>> = None;

                if response == "Changed" {
                    if body["items_changed"].is_array() {
                        queue = serde_json::from_value(body["items_changed"].to_owned()).ok();
                    }
                } else if response == "Subscribed" {
                    if body["items"].is_array() {
                        queue = serde_json::from_value(body["items"].to_owned()).ok();
                    }
                }

                return match queue {
                    Some(queue) => Parsed::Queue(queue),
                    None => Parsed::None
                }
            }
        }

        Parsed::None
    }
}

#[cfg(test)]
#[cfg(feature = "transport")]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::{RoonApi, Core, Svc, Services};

    #[tokio::test(flavor = "current_thread")]
    async fn it_works() {
        let info = json!({
            "extension_id": "com.theappgineer.rust-roon-api",
            "display_name": "Rust Roon API",
            "display_version": "0.1.0",
            "publisher": "The Appgineer",
            "email": "theappgineer@gmail.com"
        });
        let services = vec![Services::Transport(Transport::new())];
        let on_core_lost = move |core: &Core| {
            println!("Core lost: {}", core.display_name);
        };
        let mut roon = RoonApi::new(info, Box::new(on_core_lost));
        let provided: HashMap<String, Svc> = HashMap::new();
        let (mut handles, mut core_rx) = roon.start_discovery(provided, Some(services)).await.unwrap();

        handles.push(tokio::spawn(async move {
            let mut transport = None;

            loop {
                if let Some((mut core, msg)) = core_rx.recv().await {
                    if let Some(core) = core.as_mut() {
                        println!("Core found: {}, version {}", core.display_name, core.display_version);

                        transport = if let Some(transport) = core.get_transport() {
                            transport.subscribe_zones().await;
                            transport.subscribe_outputs().await;

                            Some(transport.clone())
                        } else {
                            None
                        }
                    }

                    if let Some((_, parsed)) = msg {
                        match parsed {
                            Parsed::Zones(_zones) => (),
                            Parsed::ZonesSeek(_zones_seek) => (),
                            Parsed::Outputs(outputs) => {
                                let output_id = &outputs[0].output_id;
    
                                if let Some(transport) = transport.as_mut() {
                                    transport.subscribe_queue(&output_id, 20).await;
                                }
                            }
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

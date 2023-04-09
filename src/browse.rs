use serde::Deserialize;

use crate::{Moo, Parsed};

pub const SVCNAME: &str = "com.roonlabs.browse:1";

#[derive(Debug, Deserialize)]
pub struct List {
    pub title: String,
    pub count: u32,
    pub level: u32,
    pub subtitle: Option<String>,
    pub image_key: Option<String>,
    pub display_offset: Option<u32>,
    pub hint: Option<String>
}

#[derive(Debug, Deserialize)]
pub struct Item {
    pub title: String,
    pub subtitle: Option<String>,
    pub image_key: Option<String>,
    pub item_key: Option<String>,
    pub hint: Option<String>
}

#[derive(Clone, Debug)]
pub struct Browse {
    moo: Option<Moo>
}

impl Browse {
    pub fn new() -> Self {
        Self {
            moo: None
        }
    }

    pub fn set_moo(&mut self, moo: Moo) {
        self.moo = Some(moo);
    }

    pub async fn browse(&self, opts: serde_json::Value) -> Option<usize> {
        if let Some(moo) = &self.moo {
            moo.send_req(SVCNAME.to_owned() + "/browse", Some(opts)).await.ok()
        } else {
            None
        }
    }

    pub async fn load(&self, opts: serde_json::Value) -> Option<usize> {
        if let Some(moo) = &self.moo {
            moo.send_req(SVCNAME.to_owned() + "/load", Some(opts)).await.ok()
        } else {
            None
        }
    }

    pub fn parse_msg(&self, msg: &serde_json::Value) -> Parsed {
        let body = msg["body"].to_owned();

        if body["action"] == "list" {
            let list: Option<List> = serde_json::from_value(body["list"].to_owned()).ok();

            return match list {
                Some(list) => Parsed::List(list),
                None => Parsed::None
            }
        } else if body["items"].is_array() {
            let items: Option<Vec<Item>> = serde_json::from_value(body["items"].to_owned()).ok();

            return match items {
                Some(items) => Parsed::Items(items),
                None => Parsed::None
            }
        }

        Parsed::None
    }
}

#[cfg(test)]
#[cfg(feature = "browse")]
mod tests {
    use std::collections::HashMap;
    use serde_json::json;

    use super::*;
    use crate::{RoonApi, CoreEvent, Svc, Services, ROON_API_VERSION};

    #[tokio::test(flavor = "current_thread")]
    async fn it_works() {
        let info = json!({
            "extension_id": "com.theappgineer.rust-roon-api",
            "display_name": "Rust Roon API",
            "display_version": ROON_API_VERSION,
            "publisher": "The Appgineer",
            "email": "theappgineer@gmail.com"
        });
        let mut roon = RoonApi::new(info);
        let services = vec![Services::Browse(Browse::new())];
        let provided: HashMap<String, Svc> = HashMap::new();
        let (mut handles, mut core_rx) = roon.start_discovery(provided, Some(services)).await.unwrap();

        handles.push(tokio::spawn(async move {
            let mut browse = None;

            loop {
                if let Some((core, msg)) = core_rx.recv().await {
                    match core {
                        CoreEvent::Found(mut core) => {
                            println!("Core found: {}, version {}", core.display_name, core.display_version);

                            browse = if let Some(browse) = core.get_browse() {
                                let opts = json!({
                                    "hierarchy": "browse",
                                    "pop_all": "true"
                                });

                                browse.browse(opts).await;

                                Some(browse.clone())
                            } else {
                                None
                            }
                        }
                        CoreEvent::Lost(core) => {
                            println!("Core lost: {}, version {}", core.display_name, core.display_version);
                        }
                        _ => ()
                    }

                    if let Some((_, parsed)) = msg {
                        match parsed {
                            Parsed::List(list) => {
                                if let Some(browse) = browse.as_ref() {
                                    let offset = list.display_offset.unwrap_or(0);
                                    let opts = json!({
                                        "hierarchy": "browse",
                                        "offset": offset,
                                        "set_display_offset": offset
                                    });
    
                                    browse.load(opts).await;
                                }
                            }
                            Parsed::Items(items) => {
                                for item in items {
                                    println!("{}", item.title);
                                }
                            }
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

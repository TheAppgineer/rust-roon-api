use serde::Serialize;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::Mutex;

use crate::{moo::ContentType, Moo, Parsed, RoonApiError};

pub const SVCNAME: &str = "com.roonlabs.image:1";

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scale {
    #[default]
    Original,
    Fit,
    Fill,
    Stretch,
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    #[default]
    #[serde(rename = "image/jpeg")]
    Jpeg,
    #[serde(rename = "image/png")]
    Png,
}

#[derive(Clone, Default, Serialize)]
pub struct Scaling {
    scale: Scale,
    width: u32,
    height: u32,
}

#[derive(Clone, Default, Serialize)]
pub struct Args {
    #[serde(flatten)]
    scaling: Option<Scaling>,
    format: Option<Format>,
}

impl Scaling {
    pub fn new(scale: Scale, width: u32, height: u32) -> Self {
        Self {
            scale,
            width,
            height,
        }
    }
}

impl Args {
    pub fn new(scaling: Option<Scaling>, format: Option<Format>) -> Self {
        Self { scaling, format }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Image {
    moo: Option<Moo>,
    pending: Arc<Mutex<HashMap<String, Option<usize>>>>,
}

impl Image {
    pub fn new() -> Self {
        Self {
            moo: None,
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn set_moo(&mut self, moo: Moo) {
        self.moo = Some(moo);
    }

    pub async fn get_image(&self, image_key: &str, args: Args) -> Option<usize> {
        let req_id = {
            let mut pending = self.pending.lock().await;

            if pending.contains_key(image_key) {
                Some(pending.get(image_key).cloned().flatten()?)
            } else {
                pending.insert(image_key.to_owned(), None);

                None
            }
        };

        match req_id {
            None => {
                let moo = self.moo.as_ref()?;
                let mut args = args.serialize(serde_json::value::Serializer).unwrap();

                args["image_key"] = image_key.into();

                let req_id = moo
                    .send_req(SVCNAME.to_owned() + "/get_image", Some(args))
                    .await
                    .ok();

                self.pending
                    .lock()
                    .await
                    .insert(image_key.to_owned(), req_id);

                req_id
            }
            _ => req_id,
        }
    }

    pub async fn parse_msg(&self, msg: &serde_json::Value, body: &ContentType) -> Option<Parsed> {
        let req_id = msg["request_id"]
            .as_str()
            .unwrap()
            .parse::<usize>()
            .unwrap();
        let mut pending = self.pending.lock().await;
        let image_key = pending.iter().find_map(|(key, value)| {
            if (*value)? == req_id {
                Some(key.to_owned())
            } else {
                None
            }
        })?;

        pending.remove(&image_key);

        if msg["name"] == "UnexpectedError" {
            return Some(Parsed::Error(RoonApiError::ImageUnexpectedError((req_id, image_key))));
        }

        let parsed = {
            match body {
                ContentType::Jpeg(jpeg) => {
                    Some(Parsed::Jpeg((image_key.to_owned(), jpeg.to_owned())))
                }
                ContentType::Png(png) => {
                    Some(Parsed::Png((image_key.to_owned(), png.to_owned())))
                }
                _ => None,
            }
        };

        parsed
    }
}

#[cfg(test)]
#[cfg(all(feature = "image", feature = "browse"))]
mod tests {
    use std::{collections::HashMap, fs};

    use super::*;
    use crate::{
        browse::{Action, Browse, BrowseOpts, LoadOpts},
        info, CoreEvent, Info, Parsed, RoonApi, Services, Svc,
    };

    #[tokio::test(flavor = "current_thread")]
    async fn it_works() {
        const CONFIG_PATH: &str = "config.json";
        let info = info!("com.theappgineer", "Rust Roon API");

        simple_logging::log_to_stderr(log::LevelFilter::Info);

        let mut roon = RoonApi::new(info);
        let services = vec![
            Services::Browse(Browse::new()),
            Services::Image(Image::new()),
        ];
        let provided: HashMap<String, Svc> = HashMap::new();
        let get_roon_state = || RoonApi::load_roon_state(CONFIG_PATH);
        let (mut handles, mut core_rx) = roon
            .start_discovery(Box::new(get_roon_state), provided, Some(services))
            .await
            .unwrap();

        handles.spawn(async move {
            let mut image = None;
            let mut browse = None;

            loop {
                if let Some((core, msg)) = core_rx.recv().await {
                    match core {
                        CoreEvent::Registered(mut core) => {
                            log::info!(
                                "Core registered: {}, version {}",
                                core.display_name,
                                core.display_version
                            );

                            image = core.get_image().cloned();
                            browse = core.get_browse().cloned();

                            if let Some(browse) = browse.as_ref() {
                                let opts = BrowseOpts {
                                    pop_all: true,
                                    ..Default::default()
                                };

                                browse.browse(&opts).await;
                            }
                        }
                        CoreEvent::Lost(core) => {
                            log::warn!(
                                "Core lost: {}, version {}",
                                core.display_name,
                                core.display_version
                            );
                        }
                        _ => (),
                    }

                    if let Some((_, parsed)) = msg {
                        match parsed {
                            Parsed::RoonState(roon_state) => {
                                RoonApi::save_roon_state(CONFIG_PATH, roon_state).unwrap();
                            }
                            Parsed::BrowseResult(result, _) => {
                                if result.action == Action::List {
                                    if let Some(browse) = browse.as_ref() {
                                        let offset = 0;
                                        let opts = LoadOpts {
                                            count: Some(10),
                                            offset,
                                            set_display_offset: offset,
                                            ..Default::default()
                                        };

                                        browse.load(&opts).await;
                                    }
                                }
                            }
                            Parsed::LoadResult(result, _) => {
                                let next = match result.list.level {
                                    0 => Some("Library"),
                                    1 => Some("Artists"),
                                    _ => None,
                                };
                                let item_key = next
                                    .map(|title| {
                                        result
                                            .items
                                            .iter()
                                            .find_map(|item| {
                                                if item.title == title {
                                                    Some(item.item_key.as_ref())
                                                } else {
                                                    None
                                                }
                                            })
                                            .flatten()
                                    })
                                    .flatten()
                                    .cloned();

                                match item_key {
                                    Some(_) => {
                                        if let Some(browse) = browse.as_ref() {
                                            let opts = BrowseOpts {
                                                item_key,
                                                ..Default::default()
                                            };

                                            browse.browse(&opts).await;
                                        }
                                    }
                                    None => {
                                        for item in result.items {
                                            get_image(image.as_mut(), item.image_key.as_deref())
                                                .await;
                                        }
                                    }
                                }
                            }
                            Parsed::Jpeg((image_key, jpeg)) => {
                                fs::write(format!("{image_key}.jpg"), jpeg).unwrap();
                            }
                            Parsed::Error(err) => log::error!("{}", err),
                            _ => {}
                        }
                    }
                }
            }
        });

        handles.join_next().await;
    }

    async fn get_image(image: Option<&mut Image>, image_key: Option<&str>) -> Option<()> {
        let scaling = Scaling::new(Scale::Fit, 200, 200);
        let args = Args::new(Some(scaling), Some(Format::Jpeg));

        image?.get_image(image_key?, args).await;

        Some(())
    }
}

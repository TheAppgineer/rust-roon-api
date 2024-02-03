use serde::Serialize;
use std::sync::Arc;
use tokio::sync::Mutex;
use crate::{moo::ContentType, Moo, Parsed};

pub const SVCNAME: &str = "com.roonlabs.image:1";

#[derive(Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scale {
    #[default] Original,
    Fit,
    Fill,
    Stretch,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    #[default] #[serde(rename = "image/jpeg")] Jpeg,
    #[serde(rename = "image/png")] Png,
}

#[derive(Default, Serialize)]
pub struct Scaling {
    scale: Scale,
    width: u32,
    height: u32,
}

#[derive(Default, Serialize)]
pub struct Args {
    #[serde(flatten)] scaling: Option<Scaling>,
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
        Self {
            scaling,
            format,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Image {
    moo: Option<Moo>,
    pending: Arc<Mutex<Option<(usize, String)>>>,
}

impl Image {
    pub fn new() -> Self {
        Self {
            moo: None,
            pending: Arc::new(Mutex::new(None)),
        }
    }

    pub fn set_moo(&mut self, moo: Moo) {
        self.moo = Some(moo);
    }

    pub async fn get_image(&mut self, image_key: &str, args: Args) -> Option<usize> {
        let moo = self.moo.as_ref()?;
        let mut args = args.serialize(serde_json::value::Serializer).unwrap();

        args["image_key"] = image_key.into();

        let req_id = moo.send_req(SVCNAME.to_owned() + "/get_image", Some(args)).await.ok();
        let mut pending = self.pending.lock().await;

        *pending = Some((req_id?, image_key.to_owned()));

        req_id
    }

    pub async fn parse_msg(&self, msg: &serde_json::Value, body: &ContentType) -> Option<Parsed> {
        let req_id = msg["request_id"].as_str().unwrap().parse::<usize>().unwrap();
        let pending = self.pending.lock().await;
        let parsed = if let Some((pending_id, image_key)) = pending.as_ref() {
            match body {
                ContentType::Jpeg(jpeg) if *pending_id == req_id => {
                    Some(Parsed::Jpeg((image_key.to_owned(), jpeg.to_owned())))
                }
                ContentType::Png(png) if *pending_id == req_id => {
                    Some(Parsed::Png((image_key.to_owned(), png.to_owned())))
                }
                _ => None
            }
        } else {
            None
        };

        parsed
    }
}

#[cfg(test)]
#[cfg(all(feature = "image", feature = "transport"))]
mod tests {
    use std::{collections::HashMap, fs};

    use super::*;
    use crate::{CoreEvent, Info, Parsed, RoonApi, Services, Svc, transport::{Transport, Zone}, info};

    #[tokio::test(flavor = "current_thread")]
    async fn it_works() {
        const CONFIG_PATH: &str = "config.json";
        let info = info!("com.theappgineer", "Rust Roon API");

        simple_logging::log_to_stderr(log::LevelFilter::Info);

        let mut roon = RoonApi::new(info);
        let services = vec![
            Services::Transport(Transport::new()),
            Services::Image(Image::new(),)
        ];
        let provided: HashMap<String, Svc> = HashMap::new();
        let get_roon_state = || {
            RoonApi::load_config(CONFIG_PATH, "roonstate")
        };

        let (mut handles, mut core_rx) = roon
            .start_discovery(Box::new(get_roon_state), provided, Some(services)).await.unwrap();

        handles.spawn(async move {
            let mut image = None;

            loop {
                if let Some((core, msg)) = core_rx.recv().await {
                    match core {
                        CoreEvent::Found(mut core) => {
                            log::info!("Core found: {}, version {}", core.display_name, core.display_version);

                            image = core.get_image().cloned();

                            if let Some(transport) = core.get_transport() {
                                transport.subscribe_zones().await;
                            }
                        }
                        CoreEvent::Lost(core) => {
                            log::warn!("Core lost: {}, version {}", core.display_name, core.display_version);
                        }
                        _ => ()
                    }

                    if let Some((msg, parsed)) = msg {
                        match parsed {
                            Parsed::RoonState => {
                                RoonApi::save_config(CONFIG_PATH, "roonstate", msg).unwrap();
                            }
                            Parsed::Zones(zones) => {
                                for zone in &zones {
                                    if get_image(image.as_mut(), zone).await.is_some() {
                                        break;
                                    }
                                }
                            }
                            Parsed::Jpeg((_, jpeg)) => {
                                fs::write("test.jpg", jpeg).unwrap();
                            }
                            _ => {}
                        }
                    }
                }
            }
        });

        handles.join_next().await;
    }

    async fn get_image(image: Option<&mut Image>, zone: &Zone) -> Option<()> {
        let now_playing = zone.now_playing.as_ref()?;
        let image_key = now_playing.image_key.as_deref()?;
        let scaling = Scaling::new(Scale::Fit, 200, 200);
        let args = Args::new(Some(scaling), Some(Format::Jpeg));

        image?.get_image(image_key, args).await;

        Some(())
    }
}

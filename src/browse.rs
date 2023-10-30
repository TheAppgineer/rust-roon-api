use std::{sync::Arc, collections::HashMap};
use tokio::sync::Mutex;
use serde::{Deserialize, Serialize};

use crate::{Moo, Parsed};

pub const SVCNAME: &str = "com.roonlabs.browse:1";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    None,
    Message,
    List,
    ReplaceItem,
    RemoveItem,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ListHint {
    #[serde(rename = "null")] None,
    ActionList,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ItemHint {
    #[serde(rename = "null")] None,
    Action,
    ActionList,
    List,
    Header,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct BrowseOpts {
    pub multi_session_key: Option<String>,
    pub item_key: Option<String>,
    pub input: Option<String>,
    pub zone_or_output_id: Option<String>,
    pub pop_all: bool,
    pub pop_levels: Option<u32>,
    pub refresh_list: bool,
    pub set_display_offset: Option<usize>,
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct LoadOpts {
    pub multi_session_key: Option<String>,
    pub level: Option<u32>,
    pub offset: usize,
    pub count: Option<usize>,
    pub set_display_offset: usize,
}

#[derive(Clone, Debug, Deserialize)]
pub struct List {
    pub title: String,
    pub count: usize,
    pub level: u32,
    pub subtitle: Option<String>,
    pub image_key: Option<String>,
    pub display_offset: Option<usize>,
    pub hint: Option<ListHint>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct InputPrompt {
    pub prompt: String,
    pub action: String,
    pub value: Option<String>,
    pub is_password: Option<bool>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Item {
    pub title: String,
    pub subtitle: Option<String>,
    pub image_key: Option<String>,
    pub item_key: Option<String>,
    pub hint: Option<ItemHint>,
    pub input_prompt: Option<InputPrompt>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct BrowseResult {
    pub action: Action,
    pub item: Option<Item>,
    pub list: Option<List>,
    pub message: Option<String>,
    pub is_error: Option<bool>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct LoadResult {
    pub items: Vec<Item>,
    pub offset: usize,
    pub list: List,
}

#[derive(Clone, Debug)]
pub struct Browse {
    moo: Option<Moo>,
    session_keys: Arc<Mutex<HashMap<usize, String>>>,
}

impl Browse {
    pub fn new() -> Self {
        Self {
            moo: None,
            session_keys: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn set_moo(&mut self, moo: Moo) {
        self.moo = Some(moo);
    }

    pub async fn browse(&self, opts: &BrowseOpts) -> Option<usize> {
        let moo = self.moo.as_ref()?;
        let multi_session_key = opts.multi_session_key.clone();
        let mut opts = serde_json::to_value(opts).ok()?;

        opts["hierarchy"] = "browse".into();

        let req = moo.send_req(SVCNAME.to_owned() + "/browse", Some(opts)).await.ok()?;

        if let Some(multi_session_key) = multi_session_key {
            let mut session_keys = self.session_keys.lock().await;

            session_keys.insert(req, multi_session_key);
        }

        Some(req)
    }

    pub async fn load(&self, opts: &LoadOpts) -> Option<usize> {
        let moo = self.moo.as_ref()?;
        let multi_session_key = opts.multi_session_key.clone();
        let mut opts = serde_json::to_value(opts).ok()?;

        opts["hierarchy"] = "browse".into();

        let req = moo.send_req(SVCNAME.to_owned() + "/load", Some(opts)).await.ok()?;

        if let Some(multi_session_key) = multi_session_key {
            let mut session_keys = self.session_keys.lock().await;

            session_keys.insert(req, multi_session_key);
        }

        Some(req)
    }

    pub async fn parse_msg(&self, msg: &serde_json::Value) -> Parsed {
        let req_id = msg["request_id"].as_str().unwrap().parse::<usize>().unwrap();

        if let Ok(body) = serde_json::from_value::<BrowseResult>(msg["body"].to_owned()) {
            let mut session_keys = self.session_keys.lock().await;
            let multi_session_key = session_keys.remove(&req_id);

            return Parsed::BrowseResult(body, multi_session_key);
        }

        if let Ok(body) = serde_json::from_value::<LoadResult>(msg["body"].to_owned()) {
            let mut session_keys = self.session_keys.lock().await;
            let multi_session_key = session_keys.remove(&req_id);

            return Parsed::LoadResult(body, multi_session_key);
        }

        Parsed::None
    }
}

#[cfg(test)]
#[cfg(feature = "browse")]
mod tests {
    use std::collections::HashMap;
    use tokio::io::{self, AsyncBufReadExt};

    use super::*;
    use crate::{RoonApi, CoreEvent, Info, Svc, Services, info};

    enum Input {
        Item(String),
        User((String, String)),
        NextPage,
        PrevPage
    }

    async fn display_page(result: &LoadResult) -> Option<Input> {
        let offset = result.offset;
        let entries = result.items.len();
        let mut line = String::new();

        if offset > 0 {
            println!("0 Previous");
        }

        for index in 0..entries {
            if let Some(subtitle) = result.items[index].subtitle.as_ref() {
                println!("{} {} ({})", index + 1, result.items[index].title, subtitle);
            } else {
                println!("{} {}", index + 1, result.items[index].title);
            }
        }

        if offset + entries + 1 < result.list.count {
            println!("9 Next");
        }

        println!("\nEnter the number of your choice");

        io::BufReader::new(io::stdin()).read_line(&mut line).await.unwrap();

        let input = line.chars().next().unwrap();

        match input {
            '0' => Some(Input::PrevPage),
            '1'..='8' => {
                let index = (input as usize) - ('1' as usize);
                let item = result.items.get(index)?;
                let item_key = item.item_key.as_ref()?;

                if let Some(input_prompt) = item.input_prompt.as_ref() {
                    println!("{} prompt: ", input_prompt.prompt);
                    line.clear();
                    io::BufReader::new(io::stdin()).read_line(&mut line).await.unwrap();

                    Some(Input::User((line.trim().to_owned(), item_key.to_owned())))
                } else {
                    Some(Input::Item(item_key.to_owned()))
                }
            }
            '9' => Some(Input::NextPage),
            _ => None
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn it_works() {
        const CONFIG_PATH: &str = "config.json";
        let info = info!("com.theappgineer", "Rust Roon API");
        let mut roon = RoonApi::new(info);
        let services = vec![Services::Browse(Browse::new())];
        let provided: HashMap<String, Svc> = HashMap::new();
        let get_roon_state = || {
            RoonApi::load_config(CONFIG_PATH, "roonstate")
        };
        let (mut handles, mut core_rx) = roon
            .start_discovery(Box::new(get_roon_state), provided, Some(services)).await.unwrap();

        handles.spawn(async move {
            const PAGE_ITEM_COUNT: usize = 8;
            let mut browse = None;

            loop {
                if let Some((core, msg)) = core_rx.recv().await {
                    match core {
                        CoreEvent::Found(mut core) => {
                            log::info!("Core found: {}, version {}", core.display_name, core.display_version);

                            browse = core.get_browse().cloned();

                            if let Some(browse) = browse.as_ref() {
                                let opts = BrowseOpts {
                                    pop_all: true,
                                    multi_session_key: Some("0".to_owned()),
                                    ..Default::default()
                                };

                                browse.browse(&opts).await;
                            }
                        }
                        CoreEvent::Lost(core) => {
                            log::warn!("Core lost: {}, version {}", core.display_name, core.display_version);
                        }
                        _ => ()
                    }

                    if let Some((msg, parsed)) = msg {
                        if let Some(browse) = browse.as_ref() {
                            match parsed {
                                Parsed::RoonState => {
                                    RoonApi::save_config(CONFIG_PATH, "roonstate", msg).unwrap();
                                }
                                Parsed::BrowseResult(result, multi_session_key) => {
                                    match result.action {
                                        Action::List => {
                                            if let Some(list) = result.list {
                                                let offset = list.display_offset.unwrap_or_default();
                                                let page_count = if list.count % PAGE_ITEM_COUNT > 0 {
                                                    list.count / PAGE_ITEM_COUNT + 1
                                                } else {
                                                    list.count / PAGE_ITEM_COUNT
                                                };
                                                let page_no = offset / PAGE_ITEM_COUNT + 1;
                                                let opts = LoadOpts {
                                                    count: Some(PAGE_ITEM_COUNT),
                                                    offset,
                                                    set_display_offset: offset,
                                                    multi_session_key,
                                                    ..Default::default()
                                                };

                                                println!("[{} (page {} of {})]", list.title, page_no, page_count);

                                                browse.load(&opts).await;
                                            }
                                        }
                                        Action::Message => {
                                            println!("{}: {}", if result.is_error.unwrap() {"Err"} else {"Msg"}, result.message.unwrap());
                                        }
                                        _ => ()
                                    }
                                }
                                Parsed::LoadResult(result, multi_session_key) => {
                                    if let Some(input) = display_page(&result).await {
                                        let mut opts = BrowseOpts {
                                            multi_session_key,
                                            ..Default::default()
                                        };

                                        match input {
                                            Input::Item(item_key) => {
                                                opts.set_display_offset = Some(result.offset);
                                                opts.item_key = Some(item_key);
                                            }
                                            Input::User((input, item_key)) => {
                                                opts.input = Some(input);
                                                opts.set_display_offset = Some(result.offset);
                                                opts.item_key = Some(item_key);
                                            }
                                            Input::NextPage => {
                                                opts.set_display_offset = Some(result.offset + PAGE_ITEM_COUNT);
                                            }
                                            Input::PrevPage => {
                                                opts.set_display_offset = Some(result.offset - PAGE_ITEM_COUNT);
                                            }
                                        }

                                        browse.browse(&opts).await;
                                    }
                                }
                                _ => ()
                            }
                        }
                    }
                }
            }
        });

        handles.join_next().await;
    }
}

use serde::{Deserialize, Serialize};

use crate::{Moo, Parsed};

pub const SVCNAME: &str = "com.roonlabs.browse:1";

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Hierarchy {
    #[default] Browse,
    Playlists,
    Settings,
    InternetRadio,
    Albums,
    Artists,
    Genres,
    Composers,
    Search
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    None,
    Message,
    List,
    ReplaceItem,
    RemoveItem
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ListHint {
    #[serde(rename = "null")] None,
    ActionList
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemHint {
    #[serde(rename = "null")] None,
    Action,
    ActionList,
    List,
    Header
}

#[derive(Debug, Default, Serialize)]
pub struct BrowseOpts {
    pub hierarchy: Hierarchy,
    pub multi_session_key: Option<String>,
    pub item_key: Option<String>,
    pub input: Option<String>,
    pub zone_or_output_id: Option<String>,
    pub pop_all: bool,
    pub pop_levels: Option<u32>,
    pub refresh_list: bool,
    pub set_display_offset: Option<usize>
}

#[derive(Debug, Default, Serialize)]
pub struct LoadOpts {
    pub hierarchy: Hierarchy,
    pub multi_session_key: Option<String>,
    pub level: Option<u32>,
    pub offset: usize,
    pub count: Option<usize>,
    pub set_display_offset: usize
}

#[derive(Debug, Deserialize)]
pub struct List {
    pub title: String,
    pub count: usize,
    pub level: u32,
    pub subtitle: Option<String>,
    pub image_key: Option<String>,
    pub display_offset: Option<usize>,
    pub hint: Option<ListHint>
}

#[derive(Clone, Debug, Deserialize)]
pub struct InputPrompt {
    pub prompt: String,
    pub action: String,
    pub value: Option<String>,
    pub is_password: Option<bool>
}

#[derive(Debug, Deserialize)]
pub struct Item {
    pub title: String,
    pub subtitle: Option<String>,
    pub image_key: Option<String>,
    pub item_key: Option<String>,
    pub hint: Option<ItemHint>,
    pub input_prompt: Option<InputPrompt>
}

#[derive(Debug, Deserialize)]
pub struct BrowseResult {
    pub action: Action,
    pub item: Option<Item>,
    pub list: Option<List>,
    pub message: Option<String>,
    pub is_error: Option<bool>
}

#[derive(Debug, Deserialize)]
pub struct LoadResult {
    pub items: Vec<Item>,
    pub offset: usize,
    pub list: List
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

    pub async fn browse(&self, opts: &BrowseOpts) -> Option<usize> {
        let moo = self.moo.as_ref()?;
        let opts = serde_json::to_value(opts).ok();

        moo.send_req(SVCNAME.to_owned() + "/browse", opts).await.ok()
    }

    pub async fn load(&self, opts: &LoadOpts) -> Option<usize> {
        let moo = self.moo.as_ref()?;
        let opts = serde_json::to_value(opts).ok();

        moo.send_req(SVCNAME.to_owned() + "/load", opts).await.ok()
    }

    pub fn parse_msg(&self, msg: &serde_json::Value) -> Parsed {
        if let Ok(body) = serde_json::from_value::<BrowseResult>(msg["body"].to_owned()) {
            return Parsed::BrowseResult(body);
        }

        if let Ok(body) = serde_json::from_value::<LoadResult>(msg["body"].to_owned()) {
            return Parsed::LoadResult(body);
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
    use crate::{RoonApi, CoreEvent, Info, LogLevel, Svc, Services, info};

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
        let mut info = info!("com.theappgineer", "Rust Roon API");

        info.set_log_level(LogLevel::None);

        let mut roon = RoonApi::new(info);
        let services = vec![Services::Browse(Browse::new())];
        let provided: HashMap<String, Svc> = HashMap::new();
        let (mut handles, mut core_rx) = roon.start_discovery(provided, Some(services)).await.unwrap();

        handles.push(tokio::spawn(async move {
            const HIERARCHY: Hierarchy = Hierarchy::Browse;
            const PAGE_ITEM_COUNT: usize = 8;
            let mut browse = None;

            loop {
                if let Some((core, msg)) = core_rx.recv().await {
                    match core {
                        CoreEvent::Found(mut core) => {
                            println!("Core found: {}, version {}", core.display_name, core.display_version);

                            browse = core.get_browse().cloned();

                            if let Some(browse) = browse.as_ref() {
                                let opts = BrowseOpts {
                                    hierarchy: HIERARCHY,
                                    pop_all: true,
                                    ..Default::default()
                                };

                                browse.browse(&opts).await;
                            }
                        }
                        CoreEvent::Lost(core) => {
                            println!("Core lost: {}, version {}", core.display_name, core.display_version);
                        }
                        _ => ()
                    }

                    if let Some((_, parsed)) = msg {
                        if let Some(browse) = browse.as_ref() {
                            match parsed {
                                Parsed::BrowseResult(result) => {
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
                                Parsed::LoadResult(result) => {
                                    if let Some(input) = display_page(&result).await {
                                        let mut opts: BrowseOpts = Default::default();

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
        }));

        for handle in handles {
            handle.await.unwrap();
        }
    }
}

//! Tab icons for Browser mode's sidebar, by the addresses pages report
//! (`monocode_core::browser::favicon`). Each address is fetched once per
//! run, off the main thread; the store notifies its observers when one
//! arrives, and an address that fails is not asked for again.

use std::{collections::HashMap, sync::Arc};

use gpui::{App, AppContext as _, Context, Entity, Global, Image, ImageFormat};
use monocode_core::browser::favicon::{self, IconFormat};

enum Icon {
    Loading,
    Ready(Arc<Image>),
    Failed,
}

pub(crate) struct Favicons {
    icons: HashMap<String, Icon>,
}

struct Store(Entity<Favicons>);
impl Global for Store {}

/// The app's one store, made on first use.
pub(crate) fn store(cx: &mut App) -> Entity<Favicons> {
    if let Some(store) = cx.try_global::<Store>() {
        return store.0.clone();
    }
    let store = cx.new(|_| Favicons {
        icons: HashMap::new(),
    });
    cx.set_global(Store(store.clone()));
    store
}

/// The icon at `url` if it has arrived; asks for it otherwise.
pub(crate) fn icon(url: &str, cx: &mut App) -> Option<Arc<Image>> {
    let store = store(cx);
    if let Some(icon) = store.read(cx).icons.get(url) {
        return match icon {
            Icon::Ready(image) => Some(image.clone()),
            Icon::Loading | Icon::Failed => None,
        };
    }
    store.update(cx, |store, cx| store.fetch(url.to_owned(), cx));
    None
}

impl Favicons {
    fn fetch(&mut self, url: String, cx: &mut Context<Self>) {
        // Tests make no requests.
        if cfg!(test) {
            self.icons.insert(url, Icon::Failed);
            return;
        }
        self.icons.insert(url.clone(), Icon::Loading);
        let work = {
            let url = url.clone();
            cx.background_executor()
                .spawn(async move { favicon::fetch(&url) })
        };
        cx.spawn(async move |this, cx| {
            let fetched = work.await;
            let _ = this.update(cx, |this, cx| {
                let icon = match fetched {
                    Ok((bytes, format)) => {
                        Icon::Ready(Arc::new(Image::from_bytes(image_format(format), bytes)))
                    }
                    Err(_) => Icon::Failed,
                };
                this.icons.insert(url, icon);
                cx.notify();
            });
        })
        .detach();
    }
}

fn image_format(format: IconFormat) -> ImageFormat {
    match format {
        IconFormat::Png => ImageFormat::Png,
        IconFormat::Jpeg => ImageFormat::Jpeg,
        IconFormat::Gif => ImageFormat::Gif,
        IconFormat::Webp => ImageFormat::Webp,
        IconFormat::Ico => ImageFormat::Ico,
        IconFormat::Bmp => ImageFormat::Bmp,
        IconFormat::Svg => ImageFormat::Svg,
    }
}

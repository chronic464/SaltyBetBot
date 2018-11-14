#![recursion_limit="256"]
#![feature(async_await, await_macro, futures_api, try_blocks, pin)]

#[macro_use]
extern crate stdweb;
#[macro_use]
extern crate stdweb_derive;
#[macro_use]
extern crate salty_bet_bot;
#[macro_use]
extern crate dominator;

use algorithm::record::Record;
use discard::DiscardOnDrop;
use std::rc::Rc;
use std::cell::RefCell;
use std::future::Future;
use salty_bet_bot::{set_panic_hook, spawn, deserialize_records, get_added_records, Message, Tab, Port, on_message, WaifuMessage};
use stdweb::{PromiseFuture, Reference, Once};
use stdweb::web::error::Error;
use stdweb::unstable::TryInto;
use futures_util::try_join;
use futures_util::stream::StreamExt;


// TODO cancellation
fn fetch(url: &str) -> PromiseFuture<String> {
    js!(
        // TODO cache ?
        // TODO integrity ?
        return fetch(chrome.runtime.getURL(@{url}), {
            credentials: "same-origin",
            mode: "same-origin"
        // TODO check HTTP status codes ?
        }).then(function (response) {
            return response.text();
        });
    ).try_into().unwrap()
}

fn create_twitch_tab() -> PromiseFuture<()> {
    js!(
        return new Promise(function (resolve, reject) {
            chrome.tabs.create({
                url: "https://www.twitch.tv/embed/saltybet/chat?darkpopout",
                active: false
            }, function (tab) {
                if (chrome.runtime.lastError != null) {
                    console.log(chrome.runtime.lastError);
                    reject(chrome.runtime.lastError);

                } else {
                    resolve();
                }
            });
        });
    ).try_into().unwrap()
}

/*fn get_twitch_tabs() -> PromiseFuture<Vec<Tab>> {
    js!(
        return new Promise(function (resolve, reject) {
            chrome.tabs.query({
                url: "https://www.twitch.tv/embed/saltybet/chat?darkpopout"
            }, function (tabs) {
                if (chrome.runtime.lastError != null) {
                    console.log(chrome.runtime.lastError);
                    reject(chrome.runtime.lastError);

                } else {
                    resolve(tabs);
                }
            });
        });
    ).try_into().unwrap()
}*/

fn remove_tabs(tabs: &[Tab]) -> PromiseFuture<()> {
    js!(
        // TODO move this into Rust ?
        var ids = @{tabs}.map(function (tab) { return tab.id; });

        return new Promise(function (resolve, reject) {
            chrome.tabs.remove(ids, function () {
                if (chrome.runtime.lastError != null) {
                    console.log(chrome.runtime.lastError);
                    reject(chrome.runtime.lastError);

                } else {
                    resolve();
                }
            });
        });
    ).try_into().unwrap()
}

/*async fn remove_twitch_tabs() -> Result<(), Error> {
    let tabs = await!(get_twitch_tabs())?;

    if tabs.len() > 0 {
        await!(remove_tabs(&tabs))?;
    }

    Ok(())
}*/


#[derive(Clone, Debug, PartialEq, Eq, ReferenceType)]
#[reference(instance_of = "IDBDatabase")]
pub struct Db(Reference);

impl Db {
    // TODO this should actually be u64
    pub fn new<F>(version: u32, upgrade_needed: F) -> PromiseFuture<Self> where F: FnOnce(Db, u32, Option<u32>) + 'static {
        js!(
            var upgrade_needed = @{Once(upgrade_needed)};

            return new Promise(function (resolve, reject) {
                var request = indexedDB.open("", @{version});

                request.onupgradeneeded = function (event) {
                    // TODO test this with oldVersion and newVersion
                    upgrade_needed(event.target.result, event.oldVersion, event.newVersion);
                };

                request.onsuccess = function (event) {
                    upgrade_needed.drop();
                    resolve(event.target.result);
                };

                request.onblocked = function () {
                    upgrade_needed.drop();
                    reject(new Error("Database is blocked"));
                };

                request.onerror = function (event) {
                    upgrade_needed.drop();
                    // TODO is this correct ?
                    reject(event);
                };
            });
        ).try_into().unwrap()
    }

    pub fn migrate(&self) {
        js! { @(no_return)
            @{self}.createObjectStore("records", { autoIncrement: true });
        }
    }

    fn get_all_records_raw(&self) -> PromiseFuture<Vec<String>> {
        js!(
            return new Promise(function (resolve, reject) {
                var request = @{self}.transaction("records", "readonly").objectStore("records").getAll();

                request.onsuccess = function (event) {
                    resolve(event.target.result);
                };

                request.onerror = function (event) {
                    // TODO is this correct ?
                    reject(event);
                };
            });
        ).try_into().unwrap()
    }

    pub async fn get_all_records(&self) -> Result<Vec<Record>, Error> {
        await!(self.get_all_records_raw()).map(|records| records.into_iter().map(|x| Record::deserialize(&x)).collect())
    }

    fn insert_records_raw(&self, records: Vec<String>) -> PromiseFuture<()> {
        js!(
            return new Promise(function (resolve, reject) {
                var transaction = @{self}.transaction("records", "readwrite");

                transaction.oncomplete = function () {
                    resolve();
                };

                transaction.onerror = function (event) {
                    // TODO is this correct ?
                    reject(event);
                };

                var store = transaction.objectStore("records");

                @{records}.forEach(function (value) {
                    store.add(value);
                });
            });
        ).try_into().unwrap()
    }

    // TODO is this '_ correct ?
    pub fn insert_records(&self, records: &[Record]) -> impl Future<Output = Result<(), Error>> + '_ {
        // TODO avoid doing anything if the len is 0 ?
        let records: Vec<String> = records.into_iter().map(Record::serialize).collect();

        async move {
            if records.len() > 0 {
                await!(self.insert_records_raw(records))?;
            }

            Ok(())
        }
    }

    pub fn delete_all_records(&self) -> PromiseFuture<()> {
        js!(
            return new Promise(function (resolve, reject) {
                var transaction = @{self}.transaction("records", "readwrite");

                transaction.oncomplete = function () {
                    resolve();
                };

                transaction.onerror = function (event) {
                    // TODO is this correct ?
                    reject(event);
                };

                var store = transaction.objectStore("records");

                store.clear();
            });
        ).try_into().unwrap()
    }
}


fn remove_value<A>(vec: &mut Vec<A>, value: A) -> bool where A: PartialEq {
    if let Some(index) = vec.iter().position(|x| *x == value) {
        vec.swap_remove(index);
        true

    } else {
        false
    }
}

fn remove_ports(ports: &mut Vec<Port>) -> impl Future<Output = Result<(), Error>> {
    let tabs: Vec<Tab> = ports.drain(..).map(|port| port.tab().unwrap()).collect();

    async move {
        if tabs.len() > 0 {
            await!(remove_tabs(&tabs))?;
        }

        Ok(())
    }
}


async fn main_future() -> Result<(), Error> {
    set_panic_hook();

    log!("Initializing...");

    let db = time!("Initializing database", {
        await!(Db::new(2, |db, _old, _new| {
            db.migrate();
        }))?
    });

    time!("Inserting default records", {
        let new_records = await!(fetch("SaltyBet Records.json"))?;
        let new_records = deserialize_records(&new_records);

        let old_records = await!(db.get_all_records())?;

        let added_records = get_added_records(old_records, new_records);

        await!(db.insert_records(added_records.as_slice()))?;

        log!("Inserted {} records", added_records.len());
    });

    // This is necessary because Chrome doesn't allow content scripts to use the tabs API
    DiscardOnDrop::leak(on_message(move |message| {
        clone!(db => async move {
            match message {
                Message::GetAllRecords => reply_result!({
                    await!(db.get_all_records())?
                }),
                Message::InsertRecords(records) => reply_result!({
                    await!(db.insert_records(records.as_slice()))?;
                }),
                Message::DeleteAllRecords => reply_result!({
                    await!(db.delete_all_records())?;
                }),
                Message::OpenTwitchChat => reply_result!({
                    // TODO maybe this is okay ?
                    //await!(remove_twitch_tabs())?;
                    await!(create_twitch_tab())?;
                }),
                Message::ServerLog(message) => reply!({
                    console!(log, message);
                }),
            }
        })
    }));

    struct State {
        salty_bet_ports: Vec<Port>,
        twitch_chat_ports: Vec<Port>,
    }

    let state = Rc::new(RefCell::new(State {
        salty_bet_ports: vec![],
        twitch_chat_ports: vec![],
    }));

    // This is necessary because Chrome doesn't allow content scripts to directly communicate with other content scripts
    // TODO auto-reload the tabs if they haven't sent a message in a while
    DiscardOnDrop::leak(Port::on_connect(move |port| {
        match port.name().as_str() {
            "saltybet" => spawn(clone!(state => async move {
                let a = {
                    let mut lock = state.borrow_mut();

                    let future = remove_ports(&mut lock.salty_bet_ports);

                    lock.salty_bet_ports.push(port.clone());

                    future
                };

                DiscardOnDrop::leak(port.on_disconnect(move |port| {
                    let mut lock = state.borrow_mut();

                    if remove_value(&mut lock.salty_bet_ports, port) {
                        if lock.salty_bet_ports.len() == 0 {
                            spawn(remove_ports(&mut lock.twitch_chat_ports));
                        }
                    }
                }));

                await!(a)
            })),

            "twitch_chat" => spawn(clone!(state => async move {
                let a = {
                    let mut lock = state.borrow_mut();

                    let future = remove_ports(&mut lock.twitch_chat_ports);

                    lock.twitch_chat_ports.push(port.clone());

                    future
                };

                DiscardOnDrop::leak(port.on_disconnect(clone!(state => move |port| {
                    let mut lock = state.borrow_mut();

                    remove_value(&mut lock.twitch_chat_ports, port);
                })));

                let b = async {
                    await!(port.messages().for_each(|message: Vec<WaifuMessage>| {
                        let lock = state.borrow();

                        assert!(lock.salty_bet_ports.len() <= 1);

                        for port in lock.salty_bet_ports.iter() {
                            port.send_message(&message);
                        }

                        async {}
                    }));

                    Ok(())
                };

                try_join!(a, b).map(|_| {})
            })),

            name => {
                panic!("Invalid port name: {}", name);
            },
        }
    }));

    log!("Background page started");

    Ok(())
}

fn main() {
    stdweb::initialize();

    spawn(main_future());

    stdweb::event_loop();
}

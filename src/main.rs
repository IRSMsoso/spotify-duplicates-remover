use dialoguer::Confirm;
use indicatif::{ProgressBar, ProgressState, ProgressStyle};
use itertools::Itertools;
use rspotify::model::{FullTrack, PlayableItem, PlaylistId, TrackId};
use rspotify::{prelude::*, scopes, AuthCodePkceSpotify, Credentials, OAuth};
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::hash::Hash;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::{io, thread};
use tokio::sync::broadcast;
use warp::Filter;

#[derive(Hash, Eq, PartialEq, Debug)]
struct UniqueTrack {
    name: String,
    artist_names: Vec<String>,
    duration: i64,
}

#[derive(Debug)]
struct TrackWithId<'a> {
    unique_track: UniqueTrack,
    id: Option<TrackId<'a>>,
}

// Returns a set of all track ids in tracks which duplicates indicates there are more than 1 of.
fn get_all_track_ids_of_duplicates<'a>(
    tracks: &[TrackWithId<'a>],
    duplicates: &HashMap<UniqueTrack, usize>,
) -> HashSet<TrackId<'a>> {
    //println!("tracks: {:?}", tracks);
    //println!("duplicates: {:?}", duplicates);
    let mut ids: HashSet<TrackId> = HashSet::new();
    for (duplicate_track, duplicates_count) in duplicates {
        if *duplicates_count > 1 {
            ids.extend(
                tracks
                    .iter()
                    .filter(|t| *duplicate_track == t.unique_track)
                    .filter_map(|t| t.id.to_owned()),
            );
        }
    }
    ids
}

// Gets the first id in tracks which duplicates indicates there are more than 1 of.
fn get_first_track_ids_of_duplicates<'a>(
    tracks: &[TrackWithId<'a>],
    duplicates: HashMap<UniqueTrack, usize>,
) -> HashSet<TrackId<'a>> {
    println!("tracks: {:?}", tracks);
    println!("duplicates: {:?}", duplicates);
    let mut ids: HashSet<TrackId> = HashSet::new();
    for (duplicate_track, duplicates_count) in duplicates {
        if duplicates_count > 1 {
            ids.extend(
                tracks
                    .iter()
                    .filter(|t| duplicate_track == t.unique_track)
                    .filter_map(|t| t.id.to_owned())
                    .next(),
            );
        }
    }
    ids
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let creds = Credentials {
        id: "c0d3798f6e5f4e6d9614ecfa9aebc43e".to_owned(),
        secret: None,
    };

    let oauth = OAuth {
        redirect_uri: "http://localhost:8537/callback".to_owned(),
        scopes: scopes!(
            "user-library-read",
            "playlist-read-private",
            "playlist-modify-public",
            "playlist-modify-private"
        ),
        ..Default::default()
    };

    let mut spotify = AuthCodePkceSpotify::new(creds, oauth);

    let url = spotify.get_authorize_url(None)?;

    open::that(url.as_str())?;

    let shared_code = Arc::new(Mutex::new(String::new()));
    let shared_state = Arc::new(Mutex::new(String::new()));
    let shared_code_cp = shared_code.clone();
    let shared_state_cp = shared_state.clone();

    let (tx, mut rx) = broadcast::channel(1usize);
    let tx_mutex = Arc::new(Mutex::new(tx));

    let callback_path = warp::path!("callback")
        .and(warp::query::<HashMap<String, String>>())
        .map(move |auth_code_received: HashMap<String, String>| {
            let mut locked_code = shared_code_cp.lock().unwrap();
            let mut locked_state = shared_state_cp.lock().unwrap();
            *locked_code = auth_code_received
                .get("code")
                .expect("Invalid Query")
                .to_owned();
            *locked_state = auth_code_received
                .get("state")
                .expect("Invalid Query")
                .to_owned();
            if let Ok(tx) = tx_mutex.lock() {
                let _ = tx.send(());
            }
            "Success! You can close this window."
        });

    let _ = warp::serve(callback_path)
        .bind_with_graceful_shutdown(([127, 0, 0, 1], 8537), async move {
            match rx.recv().await {
                Ok(_) => (),
                Err(err) => println!("Err: {:?}", err),
            };
        })
        .1
        .await;

    let auth_code = shared_code.lock().unwrap().to_owned();
    let auth_state = shared_state.lock().unwrap().to_owned();

    if spotify.get_oauth().state != auth_state {
        panic!("State mismatch");
    }

    spotify.request_token(&auth_code)?;

    println!("Successfully logged in!");
    print!("Album ID: ");
    io::stdout().flush()?;

    let mut buffer = String::new();
    let stdin = io::stdin();
    stdin.read_line(&mut buffer)?;

    buffer = buffer.trim_end().to_owned();

    let playlist = spotify.playlist(PlaylistId::from_id(&buffer)?, None, None)?;

    let total_tracks = playlist.tracks.total;

    println!("Got playlist {}", playlist.name);
    println!("Total Tracks: {}", total_tracks);
    println!("Retrieving track information...");

    let stream = spotify.playlist_items(PlaylistId::from_id(&buffer)?, None, None);

    let pb = ProgressBar::new(total_tracks as u64);
    pb.set_style(
        ProgressStyle::with_template(
            "[{elapsed_precise}] {msg} [{wide_bar:.cyan/blue}] {human_pos}/{human_len} ({eta})",
        )
        .unwrap()
        .with_key(
            "eta",
            |state: &ProgressState, w: &mut dyn std::fmt::Write| {
                write!(w, "{:.1}s", state.eta().as_secs_f64()).unwrap()
            },
        )
        .progress_chars("#>-"),
    );

    let mut full_tracks: Vec<FullTrack> = Vec::new();

    for (i, item) in stream.enumerate() {
        let track = match item {
            Ok(playlist_item) => match playlist_item.track {
                Some(playlist_item) => match playlist_item {
                    PlayableItem::Track(track) => track,
                    PlayableItem::Episode(_) => continue,
                },
                None => continue,
            },
            Err(_) => continue,
        };

        // Some tracks have empty names, empty artists, and 0 duration. Probably removed from spotify but not from playlist. Ignore them.
        if track.name.trim().is_empty() {
            continue;
        }

        full_tracks.push(track);

        pb.set_position(i as u64);

        ///////

        let track_counts = full_tracks
            .iter()
            .map(|track| UniqueTrack {
                name: track.name.to_owned(),
                artist_names: track
                    .artists
                    .iter()
                    .map(|artist| artist.name.to_owned())
                    .collect(),
                duration: track.duration.num_seconds(),
            })
            .counts();

        let count = track_counts
            .iter()
            .filter(|counts| *counts.1 > 1usize)
            .count();

        pb.set_message(format!("{} dups", count));

        ///////

        thread::sleep(Duration::from_millis(12));
    }

    pb.finish();

    println!();

    let tracks_with_id: Vec<TrackWithId> = full_tracks
        .iter()
        .map(|track| TrackWithId {
            unique_track: UniqueTrack {
                name: track.name.to_owned(),
                artist_names: track
                    .artists
                    .iter()
                    .map(|artist| artist.name.to_owned())
                    .collect(),
                duration: track.duration.num_seconds(),
            },
            id: track.id.to_owned(),
        })
        .collect();

    let track_counts = full_tracks
        .iter()
        .map(|track| UniqueTrack {
            name: track.name.to_owned(),
            artist_names: track
                .artists
                .iter()
                .map(|artist| artist.name.to_owned())
                .collect(),
            duration: track.duration.num_seconds(),
        })
        .counts();

    for (track, count) in &track_counts {
        if *count > 1 {
            println!(
                "* {} ({}) ({}s) ====> {}",
                track.name,
                track.artist_names.join(", "),
                track.duration,
                count
            );
        }
    }

    println!();
    println!();

    if track_counts.is_empty() {
        println!("Looks like no duplicates.");
        return Ok(());
    }

    println!(
        "These are the songs in which the following matched: Song Name, Arist Names, and Song\
    Duration (rounded to seconds) This means that it is possible for songs on this list to not be \
    actual duplicates if they have the same name, artists, and duration but different audio. But \
    what are the changes of that, right?\n"
    );

    let confirmation = Confirm::new()
        .with_prompt("That being said, would you like me to automatically remove the duplicates in your library?")
        .interact()
        .unwrap_or(true);

    if !confirmation {
        println!("Alrighty, see ya.");
        return Ok(());
    }

    let mut ids_to_remove: Vec<PlayableId> = Vec::new();
    let mut ids_to_add: Vec<PlayableId> = Vec::new();

    for id in get_all_track_ids_of_duplicates(&tracks_with_id, &track_counts) {
        ids_to_remove.push(PlayableId::from(id.to_owned()));
    }

    for id in get_first_track_ids_of_duplicates(&tracks_with_id, track_counts) {
        ids_to_add.push(PlayableId::from(id));
    }

    println!("Removing all duplicates..");
    spotify.playlist_remove_all_occurrences_of_items(
        PlaylistId::from_id(&buffer)?,
        ids_to_remove,
        None,
    )?;

    println!("Adding all duplicates back only once..");
    spotify.playlist_add_items(PlaylistId::from_id(&buffer)?, ids_to_add, None)?;

    Ok(())
}

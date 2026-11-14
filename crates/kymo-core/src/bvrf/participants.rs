//! Participants: with several in one recording, each is a stream of its own (its channels
//! and markers), and the channels and markers of all of them a last stream, "All
//! participants". With one, there are no streams.

use crate::{AnnotationData, Channel, ChannelId, Stream, StreamContent};

/// The stream of the channels and markers that belong to every participant.
pub(super) const ALL: &str = "All participants";

/// The type of a participant's stream.
const PARTICIPANT: &str = "Participant";

/// Where each channel and marker of a recording belongs.
#[derive(Debug)]
pub(super) struct Participants {
    /// Each channel's position in the file, in kymo's order: by stream, in file order
    /// within each (a stream's channels must be consecutive).
    pub(super) order: Vec<usize>,
    /// Each channel's name as the file writes it, in kymo's order.
    names: Vec<String>,
    /// Each channel's stream, in kymo's order; `None` without streams.
    owner: Vec<Option<usize>>,
    /// The streams' names: the participants' ids, then [`ALL`]; empty with one participant.
    streams: Vec<String>,
}

impl Participants {
    /// The participants of a recording whose header lists `listed` (the `Participants`
    /// section's ids, in its order) and whose channels, named `names`, belong to the
    /// participants `of` (their `ParticipantId`, `None` for all participants).
    ///
    /// Several participants make streams: those listed, in their order, then any a channel
    /// names that isn't listed (with a warning), then [`ALL`].
    pub(super) fn new(
        listed: &[String],
        of: &[Option<String>],
        names: Vec<String>,
        warnings: &mut Vec<String>,
    ) -> Participants {
        let mut ids: Vec<String> = Vec::new();
        for id in listed {
            if !ids.contains(id) {
                ids.push(id.clone());
            }
        }
        let mut unlisted: Vec<&str> = Vec::new();
        for id in of.iter().flatten() {
            if !ids.contains(id) {
                ids.push(id.clone());
                unlisted.push(id);
            }
        }
        if ids.len() < 2 {
            return Participants {
                order: (0..names.len()).collect(),
                owner: vec![None; names.len()],
                names,
                streams: Vec::new(),
            };
        }
        if !unlisted.is_empty() {
            warnings.push(format!(
                "channels belong to participants the Participants section doesn't list ({}); \
                 each is a participant of its own",
                unlisted.join(", ")
            ));
        }
        let all = ids.len();
        ids.push(ALL.to_owned());
        let stream_of = |c: usize| match &of[c] {
            Some(id) => ids.iter().position(|i| i == id).unwrap_or(all),
            None => all,
        };
        let order: Vec<usize> = (0..ids.len())
            .flat_map(|s| (0..names.len()).filter(move |&c| stream_of(c) == s))
            .collect();
        Participants {
            owner: order.iter().map(|&c| Some(stream_of(c))).collect(),
            names: order.iter().map(|&c| names[c].clone()).collect(),
            order,
            streams: ids,
        }
    }

    /// Whether the recording has several participants, and so streams.
    pub(super) fn several(&self) -> bool {
        !self.streams.is_empty()
    }

    /// The stream of the channels and markers of all participants ([`ALL`]), with several.
    pub(super) fn all(&self) -> Option<usize> {
        self.streams.len().checked_sub(1)
    }

    /// The stream of the participant `id`, with several.
    pub(super) fn of(&self, id: &str) -> Option<usize> {
        let all = self.all()?;
        self.streams[..all].iter().position(|s| s == id)
    }

    /// The channels of `stream`.
    pub(super) fn channels_of(&self, stream: usize) -> Vec<ChannelId> {
        self.select(|c| self.owner[c] == Some(stream))
    }

    /// The channels the file names `name`: in `stream`, or in any with `None`.
    pub(super) fn named(&self, name: &str, stream: Option<usize>) -> Vec<ChannelId> {
        self.select(|c| self.names[c] == name && stream.is_none_or(|s| self.owner[c] == Some(s)))
    }

    fn select(&self, keep: impl Fn(usize) -> bool) -> Vec<ChannelId> {
        (0..self.names.len())
            .filter(|&c| keep(c))
            .map(|c| ChannelId(c as u32))
            .collect()
    }

    /// `channels` (in file order) in kymo's order. With several participants, a
    /// participant's channel is named "{name} ({participant})", as pybvrf names them, and
    /// keeps its name alone as its name within its stream.
    pub(super) fn arrange(&self, channels: Vec<Channel>) -> Vec<Channel> {
        let mut channels: Vec<Option<Channel>> = channels.into_iter().map(Some).collect();
        let all = self.all();
        self.order
            .iter()
            .zip(&self.owner)
            .map(|(&c, owner)| {
                let mut channel = channels[c]
                    .take()
                    .unwrap_or_else(|| unreachable!("each channel is in the order once"));
                if let Some(s) = *owner
                    && Some(s) != all
                {
                    let name = std::mem::take(&mut channel.name);
                    channel.name = format!("{name} ({})", self.streams[s]);
                    channel.in_stream = Some(name);
                }
                channel
            })
            .collect()
    }

    /// The streams, with several participants: each participant's with its channels, or
    /// with only its markers if it has no channels, or not shown with neither; then [`ALL`]
    /// where it has channels or markers.
    pub(super) fn streams(&self, annotations: &[AnnotationData]) -> Vec<Stream> {
        let all = self.all();
        self.streams
            .iter()
            .enumerate()
            .filter_map(|(s, name)| {
                let channels = self.channels_of(s);
                let markers = annotations.iter().filter(|a| a.stream == Some(s)).count();
                let content = if !channels.is_empty() {
                    StreamContent::Channels(channels)
                } else if markers > 0 {
                    StreamContent::Annotations(markers)
                } else if Some(s) == all {
                    return None;
                } else {
                    StreamContent::NotShown("it has no channels or markers".into())
                };
                Some(Stream {
                    id: s as u32,
                    name: name.clone(),
                    kind: if Some(s) == all { "" } else { PARTICIPANT }.into(),
                    content,
                })
            })
            .collect()
    }
}

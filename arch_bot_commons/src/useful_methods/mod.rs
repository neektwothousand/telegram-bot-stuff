mod split_msg;
use std::path::PathBuf;

pub use split_msg::*;

use futures::{Future, TryStreamExt};
use teloxide::{
    net::Download,
    requests::Requester,
    types::{ChatId, FileMeta, Message, PhotoSize},
    Bot, RequestError,
};
use tempfile::NamedTempFile;

pub struct MessageMediaInfo<'a> {
    pub width: u32,
    pub height: u32,
    pub is_sticker: bool,
    pub is_gif: bool,
    pub is_video: bool,
    pub is_image: bool,
    pub is_sound: bool,
    pub is_voice_or_video_note: bool,
    pub is_vector_sticker: bool,
    pub file: &'a FileMeta,
}

impl MessageMediaInfo<'_> {
    pub fn is_image(&self) -> bool {
        !self.is_video && self.is_raster()
    }
    pub fn is_raster(&self) -> bool {
        !self.is_vector_sticker && !self.is_sound
    }
}

pub trait MessageStuff {
    fn text_full(&self) -> Option<&str>;
    #[allow(clippy::result_unit_err)] // i'm lazy lol
    /// On success, returns info about image/video/sound in the video,
    /// as well as bool that is `true` if it's a sticker.
    ///
    /// # Errors
    /// Returns Err(()) if there is a sticker but it's not raster.
    fn get_media_info(&self) -> Option<MessageMediaInfo<'_>>;
    fn find_biggest_photo(&self) -> Option<&PhotoSize>;
}

impl MessageStuff for Message {
    fn text_full(&self) -> Option<&str> {
        self.text().or_else(|| self.caption())
    }
    fn get_media_info(&self) -> Option<MessageMediaInfo<'_>> {
        if let Some(biggest) = self.find_biggest_photo() {
            return Some(MessageMediaInfo {
                width: biggest.width,
                height: biggest.height,
                is_sticker: false,
                is_gif: false,
                is_video: false,
                is_image: true,
                is_sound: false,
                is_voice_or_video_note: false,
                is_vector_sticker: false,
                file: &biggest.file,
            });
        }

        if let Some(sticker) = self.sticker() {
            return Some(MessageMediaInfo {
                width: sticker.width.into(),
                height: sticker.height.into(),
                is_sticker: true,
                is_gif: false,
                is_video: sticker.is_video(),
                is_sound: false,
                is_image: !sticker.is_video() && !sticker.is_animated(),
                is_voice_or_video_note: false,
                is_vector_sticker: sticker.is_animated(),
                file: &sticker.file,
            });
        }

        if let Some(video) = self.video() {
            return Some(MessageMediaInfo {
                width: video.width,
                height: video.height,
                is_sticker: false,
                is_gif: false,
                is_video: true,
                is_image: false,
                is_sound: false,
                is_voice_or_video_note: false,
                is_vector_sticker: false,
                file: &video.file,
            });
        }

        if let Some(animation) = self.animation() {
            return Some(MessageMediaInfo {
                width: animation.width,
                height: animation.height,
                is_sticker: false,
                is_video: true,
                is_gif: true,
                is_image: false,
                is_sound: false,
                is_voice_or_video_note: false,
                is_vector_sticker: false,
                file: &animation.file,
            });
        }

        if let Some(video_note) = self.video_note() {
            if let Some(thumb) = &video_note.thumb {
                return Some(MessageMediaInfo {
                    width: thumb.width,
                    height: thumb.height,
                    is_sticker: false,
                    is_video: true,
                    is_gif: false,
                    is_image: false,
                    is_sound: false,
                    is_voice_or_video_note: true,
                    is_vector_sticker: false,
                    file: &video_note.file,
                });
            }
        }

        if let Some(voice) = self.voice() {
            return Some(MessageMediaInfo {
                width: 0,
                height: 0,
                is_sticker: false,
                is_video: false,
                is_gif: false,
                is_image: false,
                is_sound: true,
                is_voice_or_video_note: true,
                is_vector_sticker: false,
                file: &voice.file,
            });
        }

        if let Some(reply_to) = self.reply_to_message() {
            return reply_to.get_media_info();
        }

        None
    }
    fn find_biggest_photo(&self) -> Option<&PhotoSize> {
        if let Some(photo_sizes) = self.photo() {
            photo_sizes.iter().max_by_key(|x| x.width + x.height)
        } else {
            None
        }
    }
}

pub trait FileStuff {
    fn is_local(&self) -> bool;
}

impl FileStuff for teloxide::types::File {
    fn is_local(&self) -> bool {
        std::path::Path::new(&self.path).is_absolute()
    }
}

pub trait BotStuff {
    fn download_file_to_vec(
        &self,
        file: &FileMeta,
        to: &mut Vec<u8>,
    ) -> impl Future<Output = Result<(), RequestError>> + Send;

    fn download_file_to_temp_or_directly(
        &self,
        file: &FileMeta,
    ) -> impl Future<Output = Result<(PathBuf, Option<NamedTempFile>), RequestError>> + Send;

    fn typing(&self, to_where: ChatId) -> impl Future<Output = Result<(), RequestError>> + Send;
}

impl BotStuff for Bot {
    async fn download_file_to_vec(
        &self,
        file: &FileMeta,
        to: &mut Vec<u8>,
    ) -> Result<(), RequestError> {
        let file = self.get_file(&file.id).await?;
        to.reserve_exact(file.size as usize);
        if file.is_local() {
            // From local bot API. Just read it as vec lmao
            let mut file = std::fs::File::open(&file.path)?;

            use std::io::Read;
            file.read_to_end(to)?;
        } else {
            let mut stream = self.download_file_stream(&file.path);

            while let Some(bytes) = stream.try_next().await? {
                to.extend_from_slice(&bytes);
            }
        }

        Ok(())
    }

    async fn download_file_to_temp_or_directly(
        &self,
        file: &FileMeta,
    ) -> Result<(PathBuf, Option<NamedTempFile>), RequestError> {
        let file = self.get_file(&file.id).await?;
        if file.is_local() {
            // If file is local, just return that.
            Ok((std::path::PathBuf::from(file.path), None))
        } else {
            // If the file is remote, make a tempfile and use that.
            let tempfile = tempfile::NamedTempFile::new()?;

            let reopened = tempfile.reopen()?;
            let mut tokio_file = tokio::fs::File::from_std(reopened);
            self.download_file(&file.path, &mut tokio_file).await?;

            Ok((tempfile.path().to_path_buf(), Some(tempfile)))
        }
    }

    async fn typing(&self, to_where: ChatId) -> Result<(), RequestError> {
        self.send_chat_action(to_where, teloxide::types::ChatAction::Typing)
            .await?;
        Ok(())
    }
}

//! `MovieClip` display object and support code.
use crate::avm1::{
    Avm1, AvmString, Object as Avm1Object, StageObject, TObject as Avm1TObject, Value as Avm1Value,
};
use crate::avm2::Activation as Avm2Activation;
use crate::avm2::{
    Avm2, Error as Avm2Error, Namespace as Avm2Namespace, Object as Avm2Object, QName as Avm2QName,
    StageObject as Avm2StageObject, TObject as Avm2TObject, Value as Avm2Value,
};
use crate::backend::audio::{PreloadStreamHandle, SoundHandle, SoundInstanceHandle};
use crate::backend::ui::MouseCursor;
use bitflags::bitflags;

use crate::avm1::activation::{Activation as Avm1Activation, ActivationIdentifier};
use crate::character::Character;
use crate::context::{ActionType, RenderContext, UpdateContext};
use crate::display_object::container::{
    dispatch_added_event_only, dispatch_added_to_stage_event_only, dispatch_removed_event,
    ChildContainer, TDisplayObjectContainer,
};
use crate::display_object::{
    Bitmap, Button, DisplayObjectBase, EditText, Graphic, MorphShapeStatic, TDisplayObject, Text,
    Video,
};
use crate::drawing::Drawing;
use crate::events::{ButtonKeyCode, ClipEvent, ClipEventResult};
use crate::font::Font;
use crate::prelude::*;
use crate::tag_utils::{self, DecodeResult, SwfMovie, SwfSlice, SwfStream};
use crate::types::{Degrees, Percent};
use crate::vminterface::{AvmObject, AvmType, Instantiator};
use gc_arena::{Collect, Gc, GcCell, MutationContext};
use smallvec::SmallVec;
use std::cell::{Ref, RefCell, RefMut};
use std::collections::HashMap;
use std::convert::TryFrom;
use std::sync::Arc;
use swf::extensions::ReadSwfExt;
use swf::{FrameLabelData, Tag};

type FrameNumber = u16;

/// Indication of what frame `run_frame` should jump to next.
#[derive(PartialEq, Eq)]
enum NextFrame {
    /// Construct and run the next frame in the clip.
    Next,

    /// Jump to the first frame in the clip.
    First,

    /// Do not construct or run any frames.
    Same,
}

/// A movie clip is a display object with its own timeline that runs independently of the root timeline.
/// The SWF19 spec calls this "Sprite" and the SWF tag defines it is "DefineSprite".
/// However, in AVM2, Sprite is a separate display object, and MovieClip is a subclass of Sprite.
///
/// (SWF19 pp. 201-203)
#[derive(Clone, Debug, Collect, Copy)]
#[collect(no_drop)]
pub struct MovieClip<'gc>(GcCell<'gc, MovieClipData<'gc>>);

#[derive(Clone, Debug, Collect)]
#[collect(no_drop)]
pub struct MovieClipData<'gc> {
    base: DisplayObjectBase<'gc>,
    static_data: Gc<'gc, MovieClipStatic>,
    tag_stream_pos: u64,
    current_frame: FrameNumber,
    #[collect(require_static)]
    audio_stream: Option<SoundInstanceHandle>,
    container: ChildContainer<'gc>,
    object: Option<AvmObject<'gc>>,
    clip_actions: Vec<ClipAction>,
    frame_scripts: Vec<Avm2FrameScript<'gc>>,
    has_button_clip_event: bool,
    flags: MovieClipFlags,
    avm2_constructor: Option<Avm2Object<'gc>>,
    drawing: Drawing,
    is_focusable: bool,
    has_focus: bool,
    enabled: bool,
    use_hand_cursor: bool,
    last_queued_script_frame: Option<FrameNumber>,
    queued_script_frame: Option<FrameNumber>,
}

impl<'gc> MovieClip<'gc> {
    #[allow(dead_code)]
    pub fn new(swf: SwfSlice, gc_context: MutationContext<'gc, '_>) -> Self {
        MovieClip(GcCell::allocate(
            gc_context,
            MovieClipData {
                base: Default::default(),
                static_data: Gc::allocate(gc_context, MovieClipStatic::empty(swf)),
                tag_stream_pos: 0,
                current_frame: 0,
                audio_stream: None,
                container: ChildContainer::new(),
                object: None,
                clip_actions: Vec::new(),
                frame_scripts: Vec::new(),
                has_button_clip_event: false,
                flags: MovieClipFlags::empty(),
                avm2_constructor: None,
                drawing: Drawing::new(),
                is_focusable: false,
                has_focus: false,
                enabled: true,
                use_hand_cursor: true,
                last_queued_script_frame: None,
                queued_script_frame: None,
            },
        ))
    }

    pub fn new_with_avm2(
        swf: SwfSlice,
        this: Avm2Object<'gc>,
        constr: Avm2Object<'gc>,
        gc_context: MutationContext<'gc, '_>,
    ) -> Self {
        MovieClip(GcCell::allocate(
            gc_context,
            MovieClipData {
                base: Default::default(),
                static_data: Gc::allocate(gc_context, MovieClipStatic::empty(swf)),
                tag_stream_pos: 0,
                current_frame: 0,
                audio_stream: None,
                container: ChildContainer::new(),
                object: Some(this.into()),
                clip_actions: Vec::new(),
                frame_scripts: Vec::new(),
                has_button_clip_event: false,
                flags: MovieClipFlags::empty(),
                avm2_constructor: Some(constr),
                drawing: Drawing::new(),
                is_focusable: false,
                has_focus: false,
                enabled: true,
                use_hand_cursor: true,
                last_queued_script_frame: None,
                queued_script_frame: None,
            },
        ))
    }

    pub fn new_with_data(
        gc_context: MutationContext<'gc, '_>,
        id: CharacterId,
        swf: SwfSlice,
        num_frames: u16,
    ) -> Self {
        MovieClip(GcCell::allocate(
            gc_context,
            MovieClipData {
                base: Default::default(),
                static_data: Gc::allocate(
                    gc_context,
                    MovieClipStatic::with_data(id, swf, num_frames),
                ),
                tag_stream_pos: 0,
                current_frame: 0,
                audio_stream: None,
                container: ChildContainer::new(),
                object: None,
                clip_actions: Vec::new(),
                frame_scripts: Vec::new(),
                has_button_clip_event: false,
                flags: MovieClipFlags::PLAYING,
                avm2_constructor: None,
                drawing: Drawing::new(),
                is_focusable: false,
                has_focus: false,
                enabled: true,
                use_hand_cursor: true,
                last_queued_script_frame: None,
                queued_script_frame: None,
            },
        ))
    }

    /// Construct a movie clip that represents an entire movie.
    pub fn from_movie(gc_context: MutationContext<'gc, '_>, movie: Arc<SwfMovie>) -> Self {
        Self::new_with_data(
            gc_context,
            0,
            movie.clone().into(),
            movie.header().num_frames,
        )
    }

    /// Replace the current MovieClip with a completely new SwfMovie.
    ///
    /// Playback will start at position zero, any existing streamed audio will
    /// be terminated, and so on. Children and AVM data will be kept across the
    /// load boundary.
    pub fn replace_with_movie(
        &mut self,
        gc_context: MutationContext<'gc, '_>,
        movie: Option<Arc<SwfMovie>>,
    ) {
        self.0
            .write(gc_context)
            .replace_with_movie(gc_context, movie)
    }

    pub fn preload(
        self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        morph_shapes: &mut fnv::FnvHashMap<CharacterId, MorphShapeStatic>,
    ) {
        use swf::TagCode;
        // TODO: Re-creating static data because preload step occurs after construction.
        // Should be able to hoist this up somewhere, or use MaybeUninit.
        let mut static_data = (&*self.0.read().static_data).clone();
        let data = self.0.read().static_data.swf.clone();
        let mut reader = data.read_from(0);
        let mut cur_frame = 1;
        let mut ids = fnv::FnvHashMap::default();
        let mut preload_stream_handle = None;
        let tag_callback = |reader: &mut SwfStream<'_>, tag_code, tag_len| match tag_code {
            TagCode::FileAttributes => {
                let attributes = reader.read_file_attributes()?;
                let avm_type = if attributes.is_action_script_3 {
                    log::warn!("This SWF contains ActionScript 3 which is not yet supported by Ruffle. The movie may not work as intended.");
                    AvmType::Avm2
                } else {
                    AvmType::Avm1
                };

                let movie = self.movie().unwrap();
                let library = context.library.library_for_movie_mut(movie);
                if let Err(e) = library.check_avm_type(avm_type) {
                    log::warn!("{}", e);
                }

                Ok(())
            }
            TagCode::CsmTextSettings => self
                .0
                .write(context.gc_context)
                .csm_text_settings(context, reader),
            TagCode::DefineBits => self
                .0
                .write(context.gc_context)
                .define_bits(context, reader, tag_len),
            TagCode::DefineBitsJpeg2 => self
                .0
                .write(context.gc_context)
                .define_bits_jpeg_2(context, reader, tag_len),
            TagCode::DefineBitsJpeg3 => self
                .0
                .write(context.gc_context)
                .define_bits_jpeg_3(context, reader, tag_len),
            TagCode::DefineBitsJpeg4 => self
                .0
                .write(context.gc_context)
                .define_bits_jpeg_4(context, reader, tag_len),
            TagCode::DefineBitsLossless => self
                .0
                .write(context.gc_context)
                .define_bits_lossless(context, reader, 1),
            TagCode::DefineBitsLossless2 => self
                .0
                .write(context.gc_context)
                .define_bits_lossless(context, reader, 2),
            TagCode::DefineButton => self
                .0
                .write(context.gc_context)
                .define_button_1(context, reader),
            TagCode::DefineButton2 => self
                .0
                .write(context.gc_context)
                .define_button_2(context, reader),
            TagCode::DefineButtonCxform => self
                .0
                .write(context.gc_context)
                .define_button_cxform(context, reader, tag_len),
            TagCode::DefineButtonSound => self
                .0
                .write(context.gc_context)
                .define_button_sound(context, reader),
            TagCode::DefineEditText => self
                .0
                .write(context.gc_context)
                .define_edit_text(context, reader),
            TagCode::DefineFont => self
                .0
                .write(context.gc_context)
                .define_font_1(context, reader),
            TagCode::DefineFont2 => self
                .0
                .write(context.gc_context)
                .define_font_2(context, reader),
            TagCode::DefineFont3 => self
                .0
                .write(context.gc_context)
                .define_font_3(context, reader),
            TagCode::DefineFont4 => self
                .0
                .write(context.gc_context)
                .define_font_4(context, reader),
            TagCode::DefineMorphShape => self.0.write(context.gc_context).define_morph_shape(
                context,
                reader,
                morph_shapes,
                1,
            ),
            TagCode::DefineMorphShape2 => self.0.write(context.gc_context).define_morph_shape(
                context,
                reader,
                morph_shapes,
                2,
            ),
            TagCode::DefineShape => self
                .0
                .write(context.gc_context)
                .define_shape(context, reader, 1),
            TagCode::DefineShape2 => self
                .0
                .write(context.gc_context)
                .define_shape(context, reader, 2),
            TagCode::DefineShape3 => self
                .0
                .write(context.gc_context)
                .define_shape(context, reader, 3),
            TagCode::DefineShape4 => self
                .0
                .write(context.gc_context)
                .define_shape(context, reader, 4),
            TagCode::DefineSound => self
                .0
                .write(context.gc_context)
                .define_sound(context, reader),
            TagCode::DefineVideoStream => self
                .0
                .write(context.gc_context)
                .define_video_stream(context, reader),
            TagCode::DefineSprite => self.0.write(context.gc_context).define_sprite(
                context,
                reader,
                tag_len,
                morph_shapes,
            ),
            TagCode::DefineText => self
                .0
                .write(context.gc_context)
                .define_text(context, reader, 1),
            TagCode::DefineText2 => self
                .0
                .write(context.gc_context)
                .define_text(context, reader, 2),
            TagCode::DoInitAction => self.do_init_action(context, reader, tag_len),
            TagCode::DoAbc => self.do_abc(context, reader, tag_len),
            TagCode::SymbolClass => self.symbol_class(context, reader),
            TagCode::DefineSceneAndFrameLabelData => {
                self.scene_and_frame_labels(reader, &mut static_data)
            }
            TagCode::ExportAssets => self
                .0
                .write(context.gc_context)
                .export_assets(context, reader),
            TagCode::FrameLabel => self.0.write(context.gc_context).frame_label(
                context,
                reader,
                tag_len,
                cur_frame,
                &mut static_data,
            ),
            TagCode::JpegTables => self
                .0
                .write(context.gc_context)
                .jpeg_tables(context, reader, tag_len),
            TagCode::PlaceObject => self.0.write(context.gc_context).preload_place_object(
                context,
                reader,
                tag_len,
                &mut ids,
                morph_shapes,
                1,
            ),
            TagCode::PlaceObject2 => self.0.write(context.gc_context).preload_place_object(
                context,
                reader,
                tag_len,
                &mut ids,
                morph_shapes,
                2,
            ),
            TagCode::PlaceObject3 => self.0.write(context.gc_context).preload_place_object(
                context,
                reader,
                tag_len,
                &mut ids,
                morph_shapes,
                3,
            ),
            TagCode::PlaceObject4 => self.0.write(context.gc_context).preload_place_object(
                context,
                reader,
                tag_len,
                &mut ids,
                morph_shapes,
                4,
            ),
            TagCode::RemoveObject => self
                .0
                .write(context.gc_context)
                .preload_remove_object(context, reader, &mut ids, 1),
            TagCode::RemoveObject2 => self
                .0
                .write(context.gc_context)
                .preload_remove_object(context, reader, &mut ids, 2),
            TagCode::ShowFrame => {
                self.0
                    .write(context.gc_context)
                    .preload_show_frame(context, reader, &mut cur_frame)
            }
            TagCode::ScriptLimits => self
                .0
                .write(context.gc_context)
                .script_limits(reader, context.avm1),
            TagCode::SoundStreamHead => self.0.write(context.gc_context).preload_sound_stream_head(
                context,
                reader,
                &mut preload_stream_handle,
                &mut static_data,
                1,
            ),
            TagCode::VideoFrame => self
                .0
                .write(context.gc_context)
                .preload_video_frame(context, reader),
            TagCode::SoundStreamHead2 => {
                self.0.write(context.gc_context).preload_sound_stream_head(
                    context,
                    reader,
                    &mut preload_stream_handle,
                    &mut static_data,
                    2,
                )
            }
            TagCode::SoundStreamBlock => {
                self.0.write(context.gc_context).preload_sound_stream_block(
                    context,
                    reader,
                    preload_stream_handle,
                    cur_frame,
                    tag_len,
                )
            }
            _ => Ok(()),
        };
        let _ = tag_utils::decode_tags(&mut reader, tag_callback, TagCode::End);

        // Finalize audio stream.
        if let Some(stream) = preload_stream_handle {
            if let Some(sound) = context.audio.preload_sound_stream_end(stream) {
                static_data.audio_stream_handle = Some(sound);
            }
        }

        self.0.write(context.gc_context).static_data =
            Gc::allocate(context.gc_context, static_data);
    }

    #[inline]
    fn do_init_action(
        self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        reader: &mut SwfStream<'_>,
        tag_len: usize,
    ) -> DecodeResult {
        let movie = self.movie().unwrap();
        let library = context.library.library_for_movie_mut(movie);
        if let Err(e) = library.check_avm_type(AvmType::Avm1) {
            log::warn!("{}", e);

            return Ok(());
        }

        // Queue the init actions.

        // TODO: Init actions are supposed to be executed once, and it gives a
        // sprite ID... how does that work?
        let _sprite_id = reader.read_u16()?;
        let slice = self
            .0
            .read()
            .static_data
            .swf
            .resize_to_reader(reader, tag_len)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "Invalid source or tag length when running init action",
                )
            })?;

        Avm1::run_stack_frame_for_init_action(
            self.into(),
            context.swf.header().version,
            slice,
            context,
        );

        Ok(())
    }

    #[inline]
    fn do_abc(
        self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        reader: &mut SwfStream<'_>,
        tag_len: usize,
    ) -> DecodeResult {
        let movie = self.movie().unwrap();
        let library = context.library.library_for_movie_mut(movie);
        if let Err(e) = library.check_avm_type(AvmType::Avm2) {
            log::warn!("{}", e);

            return Ok(());
        }

        // Queue the actions.
        // TODO: The tag reader parses the entire ABC file, instead of just
        // giving us a `SwfSlice` for later parsing, so we have to replcate the
        // *entire* parsing code here. This sucks.
        let flags = reader.read_u32()?;
        let name = reader.read_str()?.to_string_lossy(reader.encoding());
        let is_lazy_initialize = flags & 1 != 0;
        let domain = library.avm2_domain();

        // The rest of the tag is an ABC file so we can take our SwfSlice now.
        let slice = self
            .0
            .read()
            .static_data
            .swf
            .resize_to_reader(reader, tag_len)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "Invalid source or tag length when running init action",
                )
            })?;

        if let Err(e) = Avm2::load_abc(slice, &name, is_lazy_initialize, context, domain) {
            log::warn!("Error loading ABC file: {}", e);
        }

        Ok(())
    }

    #[inline]
    fn symbol_class(
        self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        reader: &mut SwfStream<'_>,
    ) -> DecodeResult {
        let movie = self
            .movie()
            .ok_or("Attempted to set symbol classes on movie without any")?;
        let mut activation = Avm2Activation::from_nothing(context.reborrow());

        let num_symbols = reader.read_u16()?;

        for _ in 0..num_symbols {
            let id = reader.read_u16()?;
            let class_name = reader.read_str()?.to_string_lossy(reader.encoding());

            if let Some(name) =
                Avm2QName::from_symbol_class(&class_name, activation.context.gc_context)
            {
                let library = activation
                    .context
                    .library
                    .library_for_movie_mut(movie.clone());
                let domain = library.avm2_domain();
                let proto = domain
                    .get_defined_value(&mut activation, name.clone())
                    .and_then(|v| v.coerce_to_object(&mut activation));

                match proto {
                    Ok(proto) => {
                        let library = activation
                            .context
                            .library
                            .library_for_movie_mut(movie.clone());

                        if id == 0 {
                            //TODO: This assumes only the root movie has `SymbolClass` tags.
                            self.set_avm2_constructor(activation.context.gc_context, Some(proto));
                        } else if let Some(Character::MovieClip(mc)) = library.character_by_id(id) {
                            mc.set_avm2_constructor(activation.context.gc_context, Some(proto))
                        } else {
                            log::warn!(
                                "Symbol class {} cannot be assigned to invalid character id {}",
                                class_name,
                                id
                            );
                        }
                    }
                    Err(e) => log::warn!(
                        "Got AVM2 error {} when attempting to assign symbol class {}",
                        e,
                        class_name
                    ),
                }
            }
        }

        Ok(())
    }

    #[inline]
    fn scene_and_frame_labels(
        self,
        reader: &mut SwfStream<'_>,
        static_data: &mut MovieClipStatic,
    ) -> DecodeResult {
        let mut sfl_data = reader.read_define_scene_and_frame_label_data()?;
        sfl_data
            .scenes
            .sort_unstable_by(|s1, s2| s1.frame_num.cmp(&s2.frame_num));

        for (i, FrameLabelData { frame_num, label }) in sfl_data.scenes.iter().enumerate() {
            let start = *frame_num as u16 + 1;
            let end = sfl_data
                .scenes
                .get(i + 1)
                .map(|fld| fld.frame_num + 1)
                .unwrap_or_else(|| static_data.total_frames as u32 + 1);

            let label = label.to_string_lossy(reader.encoding());
            static_data.scene_labels.insert(
                label.clone(),
                Scene {
                    name: label,
                    start,
                    length: end as u16 - start as u16,
                },
            );
        }

        for FrameLabelData { frame_num, label } in sfl_data.frame_labels {
            static_data.frame_labels.insert(
                label.to_string_lossy(reader.encoding()),
                frame_num as u16 + 1,
            );
        }

        Ok(())
    }

    #[allow(dead_code)]
    pub fn playing(self) -> bool {
        self.0.read().playing()
    }

    pub fn programmatically_played(self) -> bool {
        self.0.read().programmatically_played()
    }

    pub fn set_programmatically_played(self, mc: MutationContext<'gc, '_>) {
        self.0.write(mc).set_programmatically_played()
    }

    pub fn next_frame(self, context: &mut UpdateContext<'_, 'gc, '_>) {
        if self.current_frame() < self.total_frames() {
            self.goto_frame(context, self.current_frame() + 1, true);
        }
    }

    pub fn play(self, context: &mut UpdateContext<'_, 'gc, '_>) {
        self.0.write(context.gc_context).play()
    }

    pub fn prev_frame(self, context: &mut UpdateContext<'_, 'gc, '_>) {
        if self.current_frame() > 1 {
            self.goto_frame(context, self.current_frame() - 1, true);
        }
    }

    pub fn stop(self, context: &mut UpdateContext<'_, 'gc, '_>) {
        self.0.write(context.gc_context).stop(context)
    }

    /// Queues up a goto to the specified frame.
    /// `frame` should be 1-based.
    ///
    /// This is treated as an 'explicit' goto: frame scripts and other frame
    /// lifecycle events will be retriggered.
    pub fn goto_frame(
        self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        mut frame: FrameNumber,
        stop: bool,
    ) {
        // Stop first, in case we need to kill and restart the stream sound.
        if stop {
            self.stop(context);
        } else {
            self.play(context);
        }

        // Clamp frame number in bounds.
        if frame < 1 {
            frame = 1;
        }

        if frame != self.current_frame() {
            self.run_goto(self.into(), context, frame, false);
        }
    }

    pub fn current_frame(self) -> FrameNumber {
        self.0.read().current_frame
    }

    /// Return the current scene.
    pub fn current_scene(self) -> Option<Scene> {
        let current_frame = self.0.read().current_frame();

        self.filter_scenes(
            |best,
             Scene {
                 name: _,
                 start,
                 length: _,
             }| {
                *start <= current_frame
                    && best
                        .map(
                            |Scene {
                                 name: _,
                                 start: best_start,
                                 length: _,
                             }| start >= best_start,
                        )
                        .unwrap_or(true)
            },
        )
    }

    /// Return the previous scene.
    pub fn previous_scene(self) -> Option<Scene> {
        let current_frame = self
            .current_scene()
            .map(
                |Scene {
                     name: _,
                     start,
                     length: _,
                 }| start,
            )
            .unwrap_or_else(|| self.current_frame());

        self.filter_scenes(
            |best,
             Scene {
                 name: _,
                 start,
                 length: _,
             }| {
                *start < current_frame
                    && best
                        .map(
                            |Scene {
                                 name: _,
                                 start: best_start,
                                 length: _,
                             }| start >= best_start,
                        )
                        .unwrap_or(true)
            },
        )
    }

    /// Return the next scene.
    pub fn next_scene(self) -> Option<Scene> {
        let current_frame = self.0.read().current_frame();

        self.filter_scenes(
            |best,
             Scene {
                 name: _,
                 start,
                 length: _,
             }| {
                *start > current_frame
                    && best
                        .map(
                            |Scene {
                                 name: _,
                                 start: best_start,
                                 length: _,
                             }| start <= best_start,
                        )
                        .unwrap_or(true)
            },
        )
    }

    /// Return all scenes in the movie.
    ///
    /// Scenes will be sorted in playback order.
    pub fn scenes(self) -> Vec<Scene> {
        let read = self.0.read();
        let mut out = Vec::new();

        for (_, scene) in read.static_data.scene_labels.iter() {
            out.push(scene.clone());
        }

        out.sort_unstable_by(
            |Scene {
                 name: _,
                 start: a,
                 length: _,
             },
             Scene {
                 name: _,
                 start: b,
                 length: _,
             }| a.cmp(b),
        );

        out
    }

    /// Scan through the list of scenes and yield the best one, if available,
    /// according to a given criterion function.
    fn filter_scenes<F>(self, mut cond: F) -> Option<Scene>
    where
        F: FnMut(Option<&Scene>, &Scene) -> bool,
    {
        let read = self.0.read();
        let mut best: Option<&Scene> = None;

        for (_, scene) in read.static_data.scene_labels.iter() {
            if cond(best, scene) {
                best = Some(scene);
            }
        }

        best.cloned()
    }

    /// Yield the current frame label as a tuple of string and frame number.
    pub fn current_label(self) -> Option<(String, FrameNumber)> {
        let read = self.0.read();
        let current_frame = read.current_frame();
        let mut best: Option<(&str, FrameNumber)> = None;

        for (label, frame) in read.static_data.frame_labels.iter() {
            if *frame > current_frame {
                continue;
            }

            if best.map(|v| *frame >= v.1).unwrap_or(true) {
                best = Some((label, *frame));
            }
        }

        best.map(|(s, fnum)| (s.to_string(), fnum))
    }

    /// Yield a list of labels and frame-numbers in the current scene.
    ///
    /// Labels are returned sorted by frame number.
    pub fn labels_in_range(self, from: FrameNumber, to: FrameNumber) -> Vec<(String, FrameNumber)> {
        let read = self.0.read();

        let mut values: Vec<(String, FrameNumber)> = read
            .static_data
            .frame_labels
            .iter()
            .filter(|(_label, frame)| **frame >= from && **frame < to)
            .map(|(label, frame)| (label.clone(), *frame))
            .collect();

        values.sort_unstable_by(|(_, framea), (_, frameb)| framea.cmp(frameb));

        values
    }

    pub fn total_frames(self) -> FrameNumber {
        self.0.read().static_data.total_frames
    }

    pub fn frames_loaded(self) -> FrameNumber {
        // TODO(Herschel): root needs to progressively stream in frames.
        self.0.read().static_data.total_frames
    }

    pub fn set_avm2_constructor(
        self,
        gc_context: MutationContext<'gc, '_>,
        prototype: Option<Avm2Object<'gc>>,
    ) {
        let mut write = self.0.write(gc_context);
        write.avm2_constructor = prototype;
    }

    pub fn frame_label_to_number(self, frame_label: &str) -> Option<FrameNumber> {
        // Frame labels are case insensitive.
        let label = frame_label.to_ascii_lowercase();
        self.0.read().static_data.frame_labels.get(&label).copied()
    }

    pub fn scene_label_to_number(self, scene_label: &str) -> Option<FrameNumber> {
        //TODO: Are scene labels also case insensitive?
        self.0
            .read()
            .static_data
            .scene_labels
            .get(scene_label)
            .map(|Scene { start, .. }| start)
            .copied()
    }

    pub fn frame_exists_within_scene(self, frame_label: &str, scene_label: &str) -> bool {
        let scene = self.scene_label_to_number(scene_label);
        let frame = self.frame_label_to_number(frame_label);

        if scene.is_none() || frame.is_none() {
            return false;
        }

        let scene = scene.unwrap();
        let frame = frame.unwrap();

        if scene <= frame {
            let mut end = self.total_frames();
            for (
                _label,
                Scene {
                    start: new_scene_start,
                    ..
                },
            ) in self.0.read().static_data.scene_labels.iter()
            {
                if *new_scene_start < end && *new_scene_start > scene {
                    end = *new_scene_start;
                }
            }

            frame < end
        } else {
            false
        }
    }

    /// Gets the clip events for this movieclip.
    pub fn clip_actions(&self) -> Ref<[ClipAction]> {
        Ref::map(self.0.read(), |mc| mc.clip_actions())
    }

    /// Sets the clip actions (a.k.a. clip events) for this movieclip.
    /// Clip actions are created in the Flash IDE by using the `onEnterFrame`
    /// tag on a movieclip instance.
    pub fn set_clip_actions(self, gc_context: MutationContext<'gc, '_>, actions: Vec<ClipAction>) {
        let mut mc = self.0.write(gc_context);
        mc.has_button_clip_event = actions.iter().any(|a| a.event.is_button_event());
        mc.set_clip_actions(actions);
    }

    /// Returns an iterator of AVM1 `DoAction` blocks on the given frame number.
    /// Used by the AVM `Call` action.
    pub fn actions_on_frame(
        self,
        _context: &mut UpdateContext<'_, 'gc, '_>,
        frame: FrameNumber,
    ) -> impl DoubleEndedIterator<Item = SwfSlice> {
        use swf::{read::Reader, TagCode};

        let mut actions: SmallVec<[SwfSlice; 2]> = SmallVec::new();

        // Iterate through this clip's tags, counting frames until we reach the target frame.
        if frame > 0 && frame <= self.total_frames() {
            let mut cur_frame = 1;
            let clip = self.0.read();
            let mut reader = clip.static_data.swf.read_from(0);
            while cur_frame <= frame && !reader.get_ref().is_empty() {
                let tag_callback = |reader: &mut Reader<'_>, tag_code, tag_len| {
                    match tag_code {
                        TagCode::ShowFrame => cur_frame += 1,
                        TagCode::DoAction if cur_frame == frame => {
                            // On the target frame, add any DoAction tags to the array.
                            if let Some(code) =
                                clip.static_data.swf.resize_to_reader(reader, tag_len)
                            {
                                actions.push(code)
                            }
                        }
                        _ => (),
                    }
                    Ok(())
                };

                let _ = tag_utils::decode_tags(&mut reader, tag_callback, TagCode::ShowFrame);
            }
        }

        actions.into_iter()
    }

    pub fn run_clip_event(
        self,
        context: &mut crate::context::UpdateContext<'_, 'gc, '_>,
        event: ClipEvent,
    ) {
        self.0
            .write(context.gc_context)
            .run_clip_event(self.into(), context, event);
    }

    /// Determine what the clip's next frame should be.
    fn determine_next_frame(self) -> NextFrame {
        if self.current_frame() < self.total_frames() {
            NextFrame::Next
        } else if self.total_frames() > 1 {
            NextFrame::First
        } else {
            NextFrame::Same
        }
    }

    fn run_frame_internal(
        self,
        self_display_object: DisplayObject<'gc>,
        context: &mut UpdateContext<'_, 'gc, '_>,
        run_display_actions: bool,
    ) {
        match self.determine_next_frame() {
            NextFrame::Next => self.0.write(context.gc_context).current_frame += 1,
            NextFrame::First => return self.run_goto(self_display_object, context, 1, true),
            NextFrame::Same => self.stop(context),
        }

        let mc = self.0.read();
        let tag_stream_start = mc.static_data.swf.as_ref().as_ptr() as u64;
        let data = mc.static_data.swf.clone();
        let mut reader = data.read_from(mc.tag_stream_pos);
        let mut has_stream_block = false;
        drop(mc);

        let vm_type = self.vm_type(context);

        use swf::TagCode;
        let tag_callback = |reader: &mut SwfStream<'_>, tag_code, tag_len| match tag_code {
            TagCode::DoAction => self.do_action(self_display_object, context, reader, tag_len),
            TagCode::PlaceObject if run_display_actions && vm_type == AvmType::Avm1 => {
                self.place_object(self_display_object, context, reader, tag_len, 1)
            }
            TagCode::PlaceObject2 if run_display_actions && vm_type == AvmType::Avm1 => {
                self.place_object(self_display_object, context, reader, tag_len, 2)
            }
            TagCode::PlaceObject3 if run_display_actions && vm_type == AvmType::Avm1 => {
                self.place_object(self_display_object, context, reader, tag_len, 3)
            }
            TagCode::PlaceObject4 if run_display_actions && vm_type == AvmType::Avm1 => {
                self.place_object(self_display_object, context, reader, tag_len, 4)
            }
            TagCode::RemoveObject if run_display_actions => self.remove_object(context, reader, 1),
            TagCode::RemoveObject2 if run_display_actions => self.remove_object(context, reader, 2),
            TagCode::SetBackgroundColor => self.set_background_color(context, reader),
            TagCode::StartSound => self.start_sound_1(context, reader),
            TagCode::SoundStreamBlock => {
                has_stream_block = true;
                self.sound_stream_block(context, reader)
            }
            _ => Ok(()),
        };
        let _ = tag_utils::decode_tags(&mut reader, tag_callback, TagCode::ShowFrame);

        self.0.write(context.gc_context).tag_stream_pos =
            reader.get_ref().as_ptr() as u64 - tag_stream_start;

        // If we are playing a streaming sound, there should(?) be a `SoundStreamBlock` on each frame.
        if !has_stream_block {
            self.0.write(context.gc_context).stop_audio_stream(context);
        }

        let frame_id = self.0.read().current_frame;
        self.0.write(context.gc_context).queued_script_frame = Some(frame_id);
    }

    /// Instantiate a given child object on the timeline at a given depth.
    #[allow(clippy::too_many_arguments)]
    fn instantiate_child(
        self,
        self_display_object: DisplayObject<'gc>,
        context: &mut UpdateContext<'_, 'gc, '_>,
        id: CharacterId,
        depth: Depth,
        place_object: &swf::PlaceObject,
        copy_previous_properties: bool,
    ) -> Option<DisplayObject<'gc>> {
        if let Ok(child) = context
            .library
            .library_for_movie_mut(self.movie().unwrap()) //TODO
            .instantiate_by_id(id, context.gc_context)
        {
            // Remove previous child from children list,
            // and add new child onto front of the list.
            let prev_child = self.replace_at_depth(context, child, depth);
            {
                // Set initial properties for child.
                child.set_instantiated_by_timeline(context.gc_context, true);
                child.set_depth(context.gc_context, depth);
                child.set_parent(context.gc_context, Some(self_display_object));
                if child.vm_type(context) == AvmType::Avm2 {
                    // In AVM2 instantiation happens before frame advance so we
                    // have to special-case that
                    child.set_place_frame(context.gc_context, self.current_frame() + 1);
                } else {
                    child.set_place_frame(context.gc_context, self.current_frame());
                }
                if copy_previous_properties {
                    if let Some(prev_child) = prev_child {
                        child.copy_display_properties_from(context.gc_context, prev_child);
                    }
                }
                // Run first frame.
                child.apply_place_object(context, self.movie(), place_object);
                child.construct_frame(context);
                child.post_instantiation(context, child, None, Instantiator::Movie, false);
                // In AVM1, children are added in `run_frame` so this is necessary.
                // In AVM2 we add them in `construct_frame` so calling this causes
                // duplicate frames
                if child.vm_type(context) == AvmType::Avm1 {
                    child.run_frame(context);
                }
            }

            dispatch_added_event_only(child, context);
            dispatch_added_to_stage_event_only(child, context);
            if let Some(prev_child) = prev_child {
                dispatch_removed_event(prev_child, context);
            }

            if let Avm2Value::Object(mut p) = self.object2() {
                if let Avm2Value::Object(c) = child.object2() {
                    let name = Avm2QName::new(
                        Avm2Namespace::public(),
                        AvmString::new(context.gc_context, child.name().to_owned()),
                    );
                    let mut activation = Avm2Activation::from_nothing(context.reborrow());
                    if let Err(e) = p.init_property(p, &name, c.into(), &mut activation) {
                        log::error!(
                            "Got error when setting AVM2 child named \"{}\": {}",
                            &child.name(),
                            e
                        );
                    }
                }
            }

            Some(child)
        } else {
            log::error!("Unable to instantiate display node id {}", id);
            None
        }
    }

    pub fn run_goto(
        mut self,
        self_display_object: DisplayObject<'gc>,
        context: &mut UpdateContext<'_, 'gc, '_>,
        frame: FrameNumber,
        is_implicit: bool,
    ) {
        // Flash gotos are tricky:
        // 1) Conceptually, a goto should act like the playhead is advancing forward or
        //    backward to a frame.
        // 2) However, MovieClip timelines are stored as deltas from frame to frame,
        //    so for rewinds, we must restart to frame 1 and play forward.
        // 3) Objects that would persist over the goto conceptually should not be
        //    destroyed and recreated; they should keep their properties.
        //    Particularly for rewinds, the object should persist if it was created
        //      *before* the frame we are going to. (DisplayObject::place_frame).
        // 4) We want to avoid creating objects just to destroy them if they aren't on
        //    the goto frame, so we should instead aggregate the deltas into a final list
        //    of commands, and THEN modify the children as necessary.

        // This map will maintain a map of depth -> placement commands.
        // TODO: Move this to UpdateContext to avoid allocations.
        let mut goto_commands: Vec<GotoPlaceObject<'_>> = vec![];

        self.0.write(context.gc_context).stop_audio_stream(context);

        let is_rewind = if frame < self.current_frame() {
            // Because we can only step forward, we have to start at frame 1
            // when rewinding.
            self.0.write(context.gc_context).tag_stream_pos = 0;
            self.0.write(context.gc_context).current_frame = 0;

            // Remove all display objects that were created after the destination frame.
            // TODO: We want to do something like self.children.retain here,
            // but BTreeMap::retain does not exist.
            // TODO: AS3 children don't live on the depth list. Do they respect
            // or ignore GOTOs?
            let children: SmallVec<[_; 16]> = self
                .0
                .read()
                .container
                .iter_children_by_depth()
                .filter_map(|(depth, clip)| {
                    if clip.place_frame() > frame {
                        Some((depth, clip))
                    } else {
                        None
                    }
                })
                .collect();
            for (_depth, child) in children {
                if !child.placed_by_script() {
                    self.remove_child(context, child, Lists::all());
                } else {
                    self.remove_child(context, child, Lists::DEPTH);
                }
            }
            true
        } else {
            false
        };

        // Step through the intermediate frames, and aggregate the deltas of each frame.
        let mc = self.0.read();
        let tag_stream_start = mc.static_data.swf.as_ref().as_ptr() as u64;
        let mut frame_pos = mc.tag_stream_pos;
        let data = mc.static_data.swf.clone();
        let mut index = 0;

        // Sanity; let's make sure we don't seek way too far.
        // TODO: This should be self.frames_loaded() when we implement that.
        let clamped_frame = if frame <= mc.total_frames() {
            frame
        } else {
            mc.total_frames()
        };
        drop(mc);

        let mut reader = data.read_from(frame_pos);
        while self.current_frame() < clamped_frame && !reader.get_ref().is_empty() {
            self.0.write(context.gc_context).current_frame += 1;
            frame_pos = reader.get_ref().as_ptr() as u64 - tag_stream_start;

            use swf::TagCode;
            let tag_callback = |reader: &mut SwfStream<'gc>, tag_code, tag_len| match tag_code {
                TagCode::PlaceObject => {
                    index += 1;
                    let mut mc = self.0.write(context.gc_context);

                    mc.goto_place_object(reader, tag_len, 1, &mut goto_commands, is_rewind, index)
                }
                TagCode::PlaceObject2 => {
                    index += 1;
                    let mut mc = self.0.write(context.gc_context);

                    mc.goto_place_object(reader, tag_len, 2, &mut goto_commands, is_rewind, index)
                }
                TagCode::PlaceObject3 => {
                    index += 1;
                    let mut mc = self.0.write(context.gc_context);

                    mc.goto_place_object(reader, tag_len, 3, &mut goto_commands, is_rewind, index)
                }
                TagCode::PlaceObject4 => {
                    index += 1;
                    let mut mc = self.0.write(context.gc_context);

                    mc.goto_place_object(reader, tag_len, 4, &mut goto_commands, is_rewind, index)
                }
                TagCode::RemoveObject => {
                    self.goto_remove_object(reader, 1, context, &mut goto_commands, is_rewind)
                }
                TagCode::RemoveObject2 => {
                    self.goto_remove_object(reader, 2, context, &mut goto_commands, is_rewind)
                }
                _ => Ok(()),
            };
            let _ = tag_utils::decode_tags(&mut reader, tag_callback, TagCode::ShowFrame);
        }
        let hit_target_frame = self.0.read().current_frame == frame;

        // Run the list of goto commands to actually create and update the display objects.
        let run_goto_command = |clip: MovieClip<'gc>,
                                context: &mut UpdateContext<'_, 'gc, '_>,
                                params: &GotoPlaceObject<'_>| {
            let child_entry = clip.child_by_depth(params.depth());
            match child_entry {
                // Apply final delta to display parameters.
                // For rewinds, if an object was created before the final frame,
                // it will exist on the final frame as well. Re-use this object
                // instead of recreating.
                // If the ID is 0, we are modifying a previous child. Otherwise, we're replacing it.
                // If it's a rewind, we removed any dead children above, so we always
                // modify the previous child.
                Some(prev_child) if params.id() == 0 || is_rewind => {
                    prev_child.apply_place_object(context, self.movie(), &params.place_object);
                }
                _ => {
                    if let Some(child) = clip.instantiate_child(
                        self_display_object,
                        context,
                        params.id(),
                        params.depth(),
                        &params.place_object,
                        params.modifies_original_item(),
                    ) {
                        // Set the place frame to the frame where the object *would* have been placed.
                        child.set_place_frame(context.gc_context, params.frame);
                    }
                }
            }
        };

        // We have to be sure that queued actions are generated in the same order
        // as if the playhead had reached this frame normally.

        // First, sort the goto commands in the order of execution.
        // (Maybe it'd be better to keeps this list sorted as we create it?
        // Currently `swap_remove` calls futz with the order; but we could use `remove`).
        goto_commands.sort_by_key(|params| params.index);

        // Then, run frames for children that were created before this frame.
        goto_commands
            .iter()
            .filter(|params| params.frame < frame)
            .for_each(|goto| run_goto_command(self, context, goto));

        if !is_implicit {
            self.frame_constructed(context);
        }

        // Next, run the final frame for the parent clip.
        // Re-run the final frame without display tags (DoAction, StartSound, etc.)
        // Note that this only happens if the frame exists and is loaded;
        // e.g. gotoAndStop(9999) displays the final frame, but actions don't run!
        if hit_target_frame {
            self.0.write(context.gc_context).current_frame -= 1;
            self.0.write(context.gc_context).tag_stream_pos = frame_pos;
            self.run_frame_internal(self_display_object, context, false);
        } else {
            self.0.write(context.gc_context).current_frame = clamped_frame;
        }

        // Finally, run frames for children that are placed on this frame.
        goto_commands
            .iter()
            .filter(|params| params.frame >= frame)
            .for_each(|goto| run_goto_command(self, context, goto));

        if !is_implicit {
            self.root(context)
                .unwrap_or_else(|| self.into())
                .run_frame_scripts(context);
            self.exit_frame(context);
        }
    }

    fn construct_as_avm1_object(
        self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        display_object: DisplayObject<'gc>,
        init_object: Option<Avm1Object<'gc>>,
        instantiated_by: Instantiator,
        run_frame: bool,
    ) {
        //TODO: This will break horribly when AVM2 starts touching the display list
        if self.0.read().object.is_none() {
            let version = context.swf.version();
            let globals = context.avm1.global_object_cell();
            let avm1_constructor = self.0.read().get_registered_avm1_constructor(context);

            // If we are running within the AVM, this must be an immediate action.
            // If we are not, then this must be queued to be ran first-thing
            if let Some(constructor) = avm1_constructor.filter(|_| instantiated_by.is_avm()) {
                let mut activation = Avm1Activation::from_nothing(
                    context.reborrow(),
                    ActivationIdentifier::root("[Construct]"),
                    version,
                    globals,
                    self.into(),
                );

                if let Ok(prototype) = constructor
                    .get("prototype", &mut activation)
                    .map(|v| v.coerce_to_object(&mut activation))
                {
                    let object: Avm1Object<'gc> = StageObject::for_display_object(
                        activation.context.gc_context,
                        self.into(),
                        Some(prototype),
                    )
                    .into();
                    if let Some(init_object) = init_object {
                        for key in init_object.get_keys(&mut activation) {
                            if let Ok(value) = init_object.get(&key, &mut activation) {
                                let _ = object.set(&key, value, &mut activation);
                            }
                        }
                    }
                    self.0.write(activation.context.gc_context).object = Some(object.into());
                    if run_frame {
                        self.run_frame(&mut activation.context);
                    }
                    let _ = constructor.construct_on_existing(&mut activation, object, &[]);
                }

                return;
            }

            let object: Avm1Object<'gc> = StageObject::for_display_object(
                context.gc_context,
                display_object,
                Some(context.avm1.prototypes().movie_clip),
            )
            .into();
            if let Some(init_object) = init_object {
                let mut activation = Avm1Activation::from_nothing(
                    context.reborrow(),
                    ActivationIdentifier::root("[Init]"),
                    version,
                    globals,
                    self.into(),
                );

                for key in init_object.get_keys(&mut activation) {
                    if let Ok(value) = init_object.get(&key, &mut activation) {
                        let _ = object.set(&key, value, &mut activation);
                    }
                }
            }
            let mut mc = self.0.write(context.gc_context);
            mc.object = Some(object.into());

            let mut events = Vec::new();

            for clip_action in mc.clip_actions().iter() {
                match clip_action.event {
                    ClipEvent::Initialize => context.action_queue.queue_actions(
                        display_object,
                        ActionType::Initialize {
                            bytecode: clip_action.action_data.clone(),
                        },
                        false,
                    ),
                    ClipEvent::Construct => events.push(clip_action.action_data.clone()),
                    _ => (),
                }
            }

            context.action_queue.queue_actions(
                display_object,
                ActionType::Construct {
                    constructor: avm1_constructor,
                    events,
                },
                false,
            );
        }

        if run_frame {
            self.run_frame(context);
        }

        // If this text field has a variable set, initialize text field binding.
        Avm1::run_with_stack_frame_for_display_object(
            self.into(),
            context.swf.version(),
            context,
            |activation| {
                self.bind_text_field_variables(activation);
            },
        );
    }

    /// Allocate the AVM2 side of this object.
    ///
    /// This function does *not* call the constructor; it is intended that you
    /// will construct the object first before doing so. This function is
    /// intended to be called from `construct_frame`.
    fn allocate_as_avm2_object(
        self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        display_object: DisplayObject<'gc>,
    ) {
        let mut constructor = self.0.read().avm2_constructor.unwrap_or_else(|| {
            let mut activation = Avm2Activation::from_nothing(context.reborrow());
            let mut mc_proto = activation.context.avm2.prototypes().movieclip;
            mc_proto
                .get_property(
                    mc_proto,
                    &Avm2QName::new(Avm2Namespace::public(), "constructor"),
                    &mut activation,
                )
                .unwrap()
                .coerce_to_object(&mut activation)
                .unwrap()
        });

        let mut constr_thing = || {
            let mut activation = Avm2Activation::from_nothing(context.reborrow());
            let proto = constructor
                .get_property(
                    constructor,
                    &Avm2QName::new(Avm2Namespace::public(), "prototype"),
                    &mut activation,
                )?
                .coerce_to_object(&mut activation)?;
            let object = Avm2StageObject::for_display_object(
                activation.context.gc_context,
                display_object,
                proto,
            )
            .into();

            Ok(object)
        };
        let result: Result<Avm2Object<'gc>, Avm2Error> = constr_thing();

        if let Ok(object) = result {
            self.0.write(context.gc_context).object = Some(object.into());
        } else if let Err(e) = result {
            log::error!("Got {} when allocating AVM2 side of display object", e);
        }
    }

    /// Construct the AVM2 side of this object.
    ///
    /// This function does *not* allocate the object; it is intended that you
    /// will allocate the object first before doing so. This function is
    /// intended to be called from `post_instantiate`.
    fn construct_as_avm2_object(self, context: &mut UpdateContext<'_, 'gc, '_>) {
        let mut constructor = self.0.read().avm2_constructor.unwrap_or_else(|| {
            let mut activation = Avm2Activation::from_nothing(context.reborrow());
            let mut mc_proto = activation.context.avm2.prototypes().movieclip;
            mc_proto
                .get_property(
                    mc_proto,
                    &Avm2QName::new(Avm2Namespace::public(), "constructor"),
                    &mut activation,
                )
                .unwrap()
                .coerce_to_object(&mut activation)
                .unwrap()
        });

        if let Avm2Value::Object(object) = self.object2() {
            let mut constr_thing = || {
                let mut activation = Avm2Activation::from_nothing(context.reborrow());
                let proto = constructor
                    .get_property(
                        constructor,
                        &Avm2QName::new(Avm2Namespace::public(), "prototype"),
                        &mut activation,
                    )?
                    .coerce_to_object(&mut activation)?;
                constructor.call(Some(object), &[], &mut activation, Some(proto))?;

                Ok(())
            };
            let result: Result<(), Avm2Error> = constr_thing();

            if let Err(e) = result {
                log::error!("Got {} when constructing AVM2 side of display object", e);
            }
        }
    }

    pub fn register_frame_script(
        self,
        frame_id: FrameNumber,
        callable: Avm2Object<'gc>,
        context: &mut UpdateContext<'_, 'gc, '_>,
    ) {
        let mut write = self.0.write(context.gc_context);

        write
            .frame_scripts
            .push(Avm2FrameScript { frame_id, callable });
    }

    pub fn set_focusable(self, focusable: bool, context: &mut UpdateContext<'_, 'gc, '_>) {
        self.0.write(context.gc_context).is_focusable = focusable;
    }

    /// Handle a RemoveObject tag when running a goto action.
    #[inline]
    fn goto_remove_object<'a>(
        mut self,
        reader: &mut SwfStream<'a>,
        version: u8,
        context: &mut UpdateContext<'_, 'gc, '_>,
        goto_commands: &mut Vec<GotoPlaceObject<'a>>,
        is_rewind: bool,
    ) -> DecodeResult {
        let remove_object = if version == 1 {
            reader.read_remove_object_1()
        } else {
            reader.read_remove_object_2()
        }?;
        let depth = Depth::from(remove_object.depth);
        if let Some(i) = goto_commands.iter().position(|o| o.depth() == depth) {
            goto_commands.swap_remove(i);
        }
        if !is_rewind {
            // For fast-forwards, if this tag were to remove an object
            // that existed before the goto, then we can remove that child right away.
            // Don't do this for rewinds, because they conceptually
            // start from an empty display list, and we also want to examine
            // the old children to decide if they persist (place_frame <= goto_frame).
            let read = self.0.read();
            if let Some(child) = read.container.get_depth(depth) {
                if !child.placed_by_script() {
                    drop(read);
                    self.remove_child(context, child, Lists::all());
                } else {
                    drop(read);
                    self.remove_child(context, child, Lists::DEPTH);
                }
            }
        }
        Ok(())
    }

    pub fn enabled(self) -> bool {
        self.0.read().enabled
    }

    pub fn set_enabled(self, context: &mut UpdateContext<'_, 'gc, '_>, enabled: bool) {
        self.0.write(context.gc_context).enabled = enabled;
    }

    pub fn use_hand_cursor(self) -> bool {
        self.0.read().use_hand_cursor
    }

    pub fn set_use_hand_cursor(
        self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        use_hand_cursor: bool,
    ) {
        self.0.write(context.gc_context).use_hand_cursor = use_hand_cursor;
    }
}

impl<'gc> TDisplayObject<'gc> for MovieClip<'gc> {
    impl_display_object!(base);

    fn id(&self) -> CharacterId {
        self.0.read().id()
    }

    fn movie(&self) -> Option<Arc<SwfMovie>> {
        Some(self.0.read().movie())
    }

    fn swf_version(&self) -> u8 {
        self.0.read().movie().version()
    }

    /// Construct objects placed on this frame.
    fn construct_frame(&self, context: &mut UpdateContext<'_, 'gc, '_>) {
        // New children will be constructed when they are instantiated and thus
        // if we construct before our children, they'll get double-constructed.
        for child in self.iter_execution_list() {
            child.construct_frame(context);
        }

        // AVM1 code expects to execute in line with timeline instructions, so
        // it's exempted from frame construction.
        if self.vm_type(context) == AvmType::Avm2 {
            let needs_construction = if matches!(self.object2(), Avm2Value::Undefined) {
                self.allocate_as_avm2_object(context, (*self).into());

                true
            } else {
                false
            };

            if self.determine_next_frame() != NextFrame::First {
                let mc = self.0.read();
                let data = mc.static_data.swf.clone();
                let mut reader = data.read_from(mc.tag_stream_pos);
                drop(mc);

                let self_display_object: DisplayObject<'gc> = (*self).into();

                use swf::TagCode;
                let tag_callback = |reader: &mut SwfStream<'_>, tag_code, tag_len| match tag_code {
                    TagCode::PlaceObject => {
                        self.place_object(self_display_object, context, reader, tag_len, 1)
                    }
                    TagCode::PlaceObject2 => {
                        self.place_object(self_display_object, context, reader, tag_len, 2)
                    }
                    TagCode::PlaceObject3 => {
                        self.place_object(self_display_object, context, reader, tag_len, 3)
                    }
                    TagCode::PlaceObject4 => {
                        self.place_object(self_display_object, context, reader, tag_len, 4)
                    }
                    _ => Ok(()),
                };
                let _ = tag_utils::decode_tags(&mut reader, tag_callback, TagCode::ShowFrame);
            }

            if needs_construction {
                self.construct_as_avm2_object(context);
            }
        }
    }

    fn run_frame(&self, context: &mut UpdateContext<'_, 'gc, '_>) {
        // Children must run first.
        for child in self.iter_execution_list() {
            child.run_frame(context);
        }

        // Run my load/enterFrame clip event.
        let mut mc = self.0.write(context.gc_context);
        let is_load_frame = !mc.initialized();
        if is_load_frame {
            mc.run_clip_event((*self).into(), context, ClipEvent::Load);
            mc.set_initialized(true);
        } else {
            mc.run_clip_event((*self).into(), context, ClipEvent::EnterFrame);
        }
        drop(mc);

        // Run my SWF tags.
        if self.playing() {
            self.run_frame_internal((*self).into(), context, true);
        }

        if is_load_frame {
            self.0.write(context.gc_context).run_clip_postevent(
                (*self).into(),
                context,
                ClipEvent::Load,
            );
        }
    }

    fn run_frame_scripts(self, context: &mut UpdateContext<'_, 'gc, '_>) {
        let mut index = 0;
        let mut write = self.0.write(context.gc_context);
        let avm2_object = write.object.and_then(|o| o.as_avm2_object().ok());

        if let Some(avm2_object) = avm2_object {
            if let Some(frame_id) = write.queued_script_frame {
                let is_fresh_frame = write.last_queued_script_frame.is_none()
                    || write.queued_script_frame != write.last_queued_script_frame;

                write.last_queued_script_frame = Some(frame_id);
                write.queued_script_frame = None;

                if is_fresh_frame {
                    while let Some(fs) = write.frame_scripts.get(index) {
                        if fs.frame_id == frame_id {
                            let callable = fs.callable;
                            drop(write);
                            if let Err(e) = Avm2::run_stack_frame_for_callable(
                                callable,
                                Some(avm2_object),
                                &[],
                                context,
                            ) {
                                log::error!("Error occured when running AVM2 frame script: {}", e);
                            }
                            write = self.0.write(context.gc_context);
                        }

                        index += 1;
                    }
                }
            }
        }

        drop(write);

        if let Some(container) = self.as_container() {
            for child in container.iter_render_list() {
                child.run_frame_scripts(context);
            }
        }
    }

    fn render_self(&self, context: &mut RenderContext<'_, 'gc>) {
        let movie = self.movie();

        self.0.read().drawing.render(context, movie);
        self.render_children(context);
    }

    fn self_bounds(&self) -> BoundingBox {
        self.0.read().drawing.self_bounds()
    }

    fn hit_test_shape(
        &self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        point: (Twips, Twips),
        options: HitTestOptions,
    ) -> bool {
        if options.skip_invisible && !self.visible() && self.maskee().is_none() {
            return false;
        }

        if options.skip_mask && self.maskee().is_some() {
            return false;
        }

        if self.world_bounds().contains(point) {
            if let Some(masker) = self.masker() {
                if !masker.hit_test_shape(
                    context,
                    point,
                    HitTestOptions {
                        skip_mask: false,
                        skip_invisible: true,
                    },
                ) {
                    return false;
                }
            }

            let mut clip_depth = 0;

            for child in self.iter_render_list() {
                if child.clip_depth() > 0 {
                    if child.hit_test_shape(
                        context,
                        point,
                        HitTestOptions {
                            skip_mask: true,
                            skip_invisible: true,
                        },
                    ) {
                        clip_depth = 0;
                    } else {
                        clip_depth = child.clip_depth();
                    }
                } else if child.depth() > clip_depth
                    && child.hit_test_shape(context, point, options)
                {
                    return true;
                }
            }

            let local_matrix = self.global_to_local_matrix();
            let point = local_matrix * point;
            if self.0.read().drawing.hit_test(point, &local_matrix) {
                return true;
            }
        }

        false
    }

    fn mouse_pick(
        &self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        self_node: DisplayObject<'gc>,
        point: (Twips, Twips),
    ) -> Option<DisplayObject<'gc>> {
        if self.visible() {
            if let Some(masker) = self.masker() {
                if !masker.hit_test_shape(
                    context,
                    point,
                    HitTestOptions {
                        skip_mask: false,
                        skip_invisible: true,
                    },
                ) {
                    return None;
                }
            }

            if self.world_bounds().contains(point) {
                // This movieclip operates in "button mode" if it has a mouse handler,
                // either via on(..) or via property mc.onRelease, etc.
                let is_button_mode = {
                    if self.0.read().has_button_clip_event {
                        true
                    } else {
                        let mut activation = Avm1Activation::from_stub(
                            context.reborrow(),
                            ActivationIdentifier::root("[Mouse Pick]"),
                        );
                        let object = self.object().coerce_to_object(&mut activation);

                        ClipEvent::BUTTON_EVENT_METHODS
                            .iter()
                            .any(|handler| object.has_property(&mut activation, handler))
                    }
                };

                if is_button_mode
                    && self.hit_test_shape(
                        context,
                        point,
                        HitTestOptions {
                            skip_mask: self.maskee().is_none(),
                            skip_invisible: true,
                        },
                    )
                {
                    return Some(self_node);
                }
            }

            // Maybe we could skip recursing down at all if !world_bounds.contains(point),
            // but a child button can have an invisible hit area outside the parent's bounds.
            let mut hit_depth = 0;
            let mut result = None;

            for child in self.iter_render_list().rev() {
                if child.clip_depth() > 0 {
                    if result.is_some() && child.clip_depth() >= hit_depth {
                        if child.hit_test_shape(
                            context,
                            point,
                            HitTestOptions {
                                skip_mask: true,
                                skip_invisible: true,
                            },
                        ) {
                            return result;
                        } else {
                            result = None;
                        }
                    }
                } else if result.is_none() {
                    result = child.mouse_pick(context, child, point);

                    if result.is_some() {
                        hit_depth = child.depth();
                    }
                }
            }

            if result.is_some() {
                return result;
            }
        }

        None
    }

    fn mouse_cursor(&self) -> MouseCursor {
        if self.use_hand_cursor() {
            MouseCursor::Hand
        } else {
            MouseCursor::Arrow
        }
    }

    fn handle_clip_event(
        &self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        event: ClipEvent,
    ) -> ClipEventResult {
        if event.is_button_event() && !self.visible() {
            return ClipEventResult::NotHandled;
        }

        if !self.enabled()
            && event.is_button_event()
            && !matches!(event, ClipEvent::KeyPress { .. })
        {
            return ClipEventResult::NotHandled;
        }

        if event.propagates() {
            for child in self.iter_execution_list() {
                if child.handle_clip_event(context, event) == ClipEventResult::Handled {
                    return ClipEventResult::Handled;
                }
            }
        }

        self.0.read().run_clip_event((*self).into(), context, event)
    }

    fn as_movie_clip(&self) -> Option<MovieClip<'gc>> {
        Some(*self)
    }

    fn as_container(self) -> Option<DisplayObjectContainer<'gc>> {
        Some(self.into())
    }

    fn as_drawing(&self, gc_context: MutationContext<'gc, '_>) -> Option<RefMut<'_, Drawing>> {
        Some(RefMut::map(self.0.write(gc_context), |s| &mut s.drawing))
    }

    fn post_instantiation(
        &self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        display_object: DisplayObject<'gc>,
        init_object: Option<Avm1Object<'gc>>,
        instantiated_by: Instantiator,
        run_frame: bool,
    ) {
        self.set_default_instance_name(context);

        let vm_type = self.vm_type(context);
        if vm_type == AvmType::Avm1 {
            self.construct_as_avm1_object(
                context,
                display_object,
                init_object,
                instantiated_by,
                run_frame,
            );
        }
    }

    fn object(&self) -> Avm1Value<'gc> {
        self.0
            .read()
            .object
            .and_then(|o| o.as_avm1_object().ok())
            .map(Avm1Value::from)
            .unwrap_or(Avm1Value::Undefined)
    }

    fn object2(&self) -> Avm2Value<'gc> {
        self.0
            .read()
            .object
            .and_then(|o| o.as_avm2_object().ok())
            .map(Avm2Value::from)
            .unwrap_or(Avm2Value::Undefined)
    }

    fn unload(&self, context: &mut UpdateContext<'_, 'gc, '_>) {
        for child in self.iter_execution_list() {
            child.unload(context);
        }

        if let Some(node) = self.maskee() {
            node.set_masker(context.gc_context, None, true);
        } else if let Some(node) = self.masker() {
            node.set_maskee(context.gc_context, None, true);
        }

        // Unregister any text field variable bindings.
        if let Avm1Value::Object(object) = self.object() {
            if let Some(stage_object) = object.as_stage_object() {
                stage_object.unregister_text_field_bindings(context);
            }
        }

        let had_focus = self.0.read().has_focus;
        if had_focus {
            let tracker = context.focus_tracker;
            tracker.set(None, context);
        }

        {
            let mut mc = self.0.write(context.gc_context);
            mc.stop_audio_stream(context);
            mc.run_clip_event((*self).into(), context, ClipEvent::Unload);
        }
        self.set_removed(context.gc_context, true);
    }

    fn allow_as_mask(&self) -> bool {
        !self.is_empty()
    }

    fn is_focusable(&self) -> bool {
        self.0.read().is_focusable
    }

    fn on_focus_changed(&self, gc_context: MutationContext<'gc, '_>, focused: bool) {
        self.0.write(gc_context).has_focus = focused;
    }
}

impl<'gc> TDisplayObjectContainer<'gc> for MovieClip<'gc> {
    impl_display_object_container!(container);
}

impl<'gc> MovieClipData<'gc> {
    /// Replace the current MovieClipData with a completely new SwfMovie.
    ///
    /// Playback will start at position zero, any existing streamed audio will
    /// be terminated, and so on. Children and AVM data will NOT be kept across
    /// the load boundary.
    ///
    /// If no movie is provided, then the movie clip will be replaced with an
    /// empty movie of the same SWF version.
    pub fn replace_with_movie(
        &mut self,
        gc_context: MutationContext<'gc, '_>,
        movie: Option<Arc<SwfMovie>>,
    ) {
        let movie = movie.unwrap_or_else(|| Arc::new(SwfMovie::empty(self.movie().version())));
        let total_frames = movie.header().num_frames;

        self.base.reset_for_movie_load();
        self.static_data = Gc::allocate(
            gc_context,
            MovieClipStatic::with_data(0, movie.into(), total_frames),
        );
        self.tag_stream_pos = 0;
        self.flags = MovieClipFlags::PLAYING;
        self.current_frame = 0;
        self.audio_stream = None;
        self.container = ChildContainer::new();
    }

    fn id(&self) -> CharacterId {
        self.static_data.id
    }

    fn current_frame(&self) -> FrameNumber {
        self.current_frame
    }

    fn total_frames(&self) -> FrameNumber {
        self.static_data.total_frames
    }

    fn playing(&self) -> bool {
        self.flags.contains(MovieClipFlags::PLAYING)
    }

    fn set_playing(&mut self, value: bool) {
        self.flags.set(MovieClipFlags::PLAYING, value);
    }

    fn programmatically_played(&self) -> bool {
        self.flags.contains(MovieClipFlags::PROGRAMMATICALLY_PLAYED)
    }

    fn set_programmatically_played(&mut self) {
        self.flags |= MovieClipFlags::PROGRAMMATICALLY_PLAYED;
    }

    fn play(&mut self) {
        // Can only play clips with multiple frames.
        if self.total_frames() > 1 {
            self.set_playing(true);
        }
    }

    fn stop(&mut self, context: &mut UpdateContext<'_, 'gc, '_>) {
        self.set_playing(false);
        self.stop_audio_stream(context);
    }

    fn tag_stream_len(&self) -> usize {
        self.static_data.swf.end - self.static_data.swf.start
    }

    /// Handles a PlaceObject tag when running a goto action.
    #[inline]
    fn goto_place_object<'a>(
        &mut self,
        reader: &mut SwfStream<'a>,
        tag_len: usize,
        version: u8,
        goto_commands: &mut Vec<GotoPlaceObject<'a>>,
        is_rewind: bool,
        index: usize,
    ) -> DecodeResult {
        let place_object = if version == 1 {
            reader.read_place_object(tag_len)
        } else {
            reader.read_place_object_2_or_3(version)
        }?;

        // We merge the deltas from this PlaceObject with the previous command.
        let depth = Depth::from(place_object.depth);
        let mut goto_place =
            GotoPlaceObject::new(self.current_frame(), place_object, is_rewind, index);
        if let Some(i) = goto_commands.iter().position(|o| o.depth() == depth) {
            goto_commands[i].merge(&mut goto_place);
        } else {
            goto_commands.push(goto_place);
        }

        Ok(())
    }

    /// Run all actions for the given clip event.
    fn run_clip_event(
        &self,
        self_display_object: DisplayObject<'gc>,
        context: &mut UpdateContext<'_, 'gc, '_>,
        event: ClipEvent,
    ) -> ClipEventResult {
        let mut handled = ClipEventResult::NotHandled;

        if let Some(AvmObject::Avm1(object)) = self.object {
            // TODO: What's the behavior for loaded SWF files?
            if context.swf.version() >= 5 {
                for clip_action in self
                    .clip_actions
                    .iter()
                    .filter(|action| action.event == event)
                {
                    // KeyPress events are consumed by a single instance.
                    if matches!(clip_action.event, ClipEvent::KeyPress { .. }) {
                        handled = ClipEventResult::Handled;
                    }
                    context.action_queue.queue_actions(
                        self_display_object,
                        ActionType::Normal {
                            bytecode: clip_action.action_data.clone(),
                        },
                        event == ClipEvent::Unload,
                    );
                }

                // Queue ActionScript-defined event handlers after the SWF defined ones.
                // (e.g., clip.onEnterFrame = foo).
                if context.swf.version() >= 6 {
                    if let Some(name) = event.method_name() {
                        // Keyboard events don't fire their methods unless the movieclip has focus (#2120).
                        if !event.is_key_event() || self.has_focus {
                            context.action_queue.queue_actions(
                                self_display_object,
                                ActionType::Method {
                                    object,
                                    name,
                                    args: vec![],
                                },
                                event == ClipEvent::Unload,
                            );
                        }
                    }
                }
            }
        }

        handled
    }

    /// Run clip actions that trigger after the clip's own actions.
    ///
    /// Currently, this is purely limited to `MovieClipLoader`'s `onLoadInit`
    /// event, delivered via the `LoadManager`. We need to be called here so
    /// that external init code runs after the event.
    ///
    /// TODO: If it turns out other `Load` events need to be delayed, perhaps
    /// we should change which frame triggers a `Load` event, rather than
    /// making sure our actions run after the clip's.
    fn run_clip_postevent(
        &self,
        self_display_object: DisplayObject<'gc>,
        context: &mut UpdateContext<'_, 'gc, '_>,
        event: ClipEvent,
    ) {
        // Finally, queue any loaders that may be waiting for this event.
        if let ClipEvent::Load = event {
            context.load_manager.movie_clip_on_load(
                self_display_object,
                //TODO: This should have an AVM2 onload path.
                self.object.and_then(|o| o.as_avm1_object().ok()),
                context.action_queue,
            );
        }
    }

    pub fn clip_actions(&self) -> &[ClipAction] {
        &self.clip_actions
    }

    pub fn set_clip_actions(&mut self, actions: Vec<ClipAction>) {
        self.clip_actions = actions;
    }

    fn initialized(&self) -> bool {
        self.flags.contains(MovieClipFlags::INITIALIZED)
    }

    fn set_initialized(&mut self, value: bool) -> bool {
        let ret = self.flags.contains(MovieClipFlags::INITIALIZED);
        self.flags.set(MovieClipFlags::INITIALIZED, value);
        !ret
    }

    /// Stops the audio stream if one is playing.
    fn stop_audio_stream(&mut self, context: &mut UpdateContext<'_, 'gc, '_>) {
        if let Some(audio_stream) = self.audio_stream.take() {
            context.stop_sound(audio_stream);
        }
    }

    /// Fetch the avm1 constructor associated with this MovieClip by `Object.registerClass`.
    /// Return `None` if this MovieClip isn't exported, or if no constructor is associated
    /// to its symbol name.
    fn get_registered_avm1_constructor(
        &self,
        context: &mut UpdateContext<'_, 'gc, '_>,
    ) -> Option<Avm1Object<'gc>> {
        let symbol_name = self.static_data.exported_name.borrow();
        let symbol_name = symbol_name.as_ref()?;
        let library = context.library.library_for_movie_mut(self.movie());
        let registry = library.avm1_constructor_registry()?;
        let ctor = registry.get(symbol_name)?;
        Some(Avm1Object::FunctionObject(ctor))
    }

    pub fn movie(&self) -> Arc<SwfMovie> {
        self.static_data.swf.movie.clone()
    }
}

// Preloading of definition tags
impl<'gc, 'a> MovieClipData<'gc> {
    #[inline]
    fn define_bits_lossless(
        &mut self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        reader: &mut SwfStream<'a>,
        version: u8,
    ) -> DecodeResult {
        let define_bits_lossless = reader.read_define_bits_lossless(version)?;
        let bitmap_info = context
            .renderer
            .register_bitmap_png(&define_bits_lossless)?;
        let bitmap = crate::display_object::Bitmap::new(
            context,
            define_bits_lossless.id,
            bitmap_info.handle,
            bitmap_info.width,
            bitmap_info.height,
        );
        context
            .library
            .library_for_movie_mut(self.movie())
            .register_character(define_bits_lossless.id, Character::Bitmap(bitmap));
        Ok(())
    }

    #[inline]
    fn define_morph_shape(
        &mut self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        reader: &mut SwfStream<'a>,
        morph_shapes: &mut fnv::FnvHashMap<CharacterId, MorphShapeStatic>,
        version: u8,
    ) -> DecodeResult {
        // Certain backends may have to preload morph shape frames, so defer registering until the end.
        let swf_shape = reader.read_define_morph_shape(version)?;
        let morph_shape = MorphShapeStatic::from_swf_tag(context, &swf_shape, self.movie());
        morph_shapes.insert(swf_shape.id, morph_shape);
        Ok(())
    }

    #[inline]
    fn define_shape(
        &mut self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        reader: &mut SwfStream<'a>,
        version: u8,
    ) -> DecodeResult {
        let movie = self.movie();
        let swf_shape = reader.read_define_shape(version)?;
        let id = swf_shape.id;
        let graphic = Graphic::from_swf_tag(context, swf_shape, movie);
        context
            .library
            .library_for_movie_mut(self.movie())
            .register_character(id, Character::Graphic(graphic));
        Ok(())
    }

    #[inline]
    fn preload_place_object(
        &mut self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        reader: &mut SwfStream<'a>,
        tag_len: usize,
        ids: &mut fnv::FnvHashMap<Depth, CharacterId>,
        morph_shapes: &mut fnv::FnvHashMap<CharacterId, MorphShapeStatic>,
        version: u8,
    ) -> DecodeResult {
        use swf::PlaceObjectAction;
        let place_object = if version == 1 {
            reader.read_place_object(tag_len)
        } else {
            reader.read_place_object_2_or_3(version)
        }?;
        match place_object.action {
            PlaceObjectAction::Place(id) => {
                if let Some(morph_shape) = morph_shapes.get_mut(&id) {
                    ids.insert(place_object.depth.into(), id);
                    if let Some(ratio) = place_object.ratio {
                        morph_shape.register_ratio(context, ratio);
                    }
                }
            }
            PlaceObjectAction::Modify => {
                if let Some(&id) = ids.get(&place_object.depth.into()) {
                    if let Some(morph_shape) = morph_shapes.get_mut(&id) {
                        ids.insert(place_object.depth.into(), id);
                        if let Some(ratio) = place_object.ratio {
                            morph_shape.register_ratio(context, ratio);
                        }
                    }
                }
            }
            PlaceObjectAction::Replace(id) => {
                if let Some(morph_shape) = morph_shapes.get_mut(&id) {
                    ids.insert(place_object.depth.into(), id);
                    if let Some(ratio) = place_object.ratio {
                        morph_shape.register_ratio(context, ratio);
                    }
                } else {
                    ids.remove(&place_object.depth.into());
                }
            }
        };

        Ok(())
    }

    #[inline]
    fn preload_sound_stream_block(
        &mut self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        reader: &mut SwfStream<'a>,
        stream: Option<PreloadStreamHandle>,
        cur_frame: FrameNumber,
        tag_len: usize,
    ) -> DecodeResult {
        if let Some(stream) = stream {
            let data = &reader.get_ref()[..tag_len];
            context
                .audio
                .preload_sound_stream_block(stream, cur_frame, data);
        }

        Ok(())
    }

    #[inline]
    fn preload_sound_stream_head(
        &mut self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        reader: &mut SwfStream<'a>,
        stream: &mut Option<PreloadStreamHandle>,
        static_data: &mut MovieClipStatic,
        _version: u8,
    ) -> DecodeResult {
        let audio_stream_info = reader.read_sound_stream_head()?;
        *stream = context.audio.preload_sound_stream_head(&audio_stream_info);
        static_data.audio_stream_info = Some(audio_stream_info);
        Ok(())
    }

    fn csm_text_settings(
        &mut self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        reader: &mut SwfStream<'a>,
    ) -> DecodeResult {
        let settings = reader.read_csm_text_settings()?;
        let library = context.library.library_for_movie_mut(self.movie());
        match library.character_by_id(settings.id) {
            Some(Character::Text(text)) => {
                text.set_render_settings(context.gc_context, settings.into());
            }
            Some(Character::EditText(edit_text)) => {
                edit_text.set_render_settings(context.gc_context, settings.into());
            }
            Some(_) => {
                log::warn!(
                    "Tried to apply CSMTextSettings to non-text character ID {}",
                    settings.id
                );
            }
            None => {
                log::warn!(
                    "Tried to apply CSMTextSettings to unregistered character ID {}",
                    settings.id
                );
            }
        }
        Ok(())
    }

    #[inline]
    fn preload_video_frame(
        &mut self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        reader: &mut SwfStream,
    ) -> DecodeResult {
        match reader.read_video_frame()? {
            Tag::VideoFrame(vframe) => {
                let library = context.library.library_for_movie_mut(self.movie());
                match library.character_by_id(vframe.stream_id) {
                    Some(Character::Video(mut v)) => {
                        v.preload_swf_frame(vframe, context);

                        Ok(())
                    }
                    _ => Err(format!(
                        "Attempted to preload video frames into non-video character {}",
                        vframe.stream_id
                    )
                    .into()),
                }
            }
            _ => unreachable!(),
        }
    }

    #[inline]
    fn define_bits(
        &mut self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        reader: &mut SwfStream<'a>,
        tag_len: usize,
    ) -> DecodeResult {
        use std::io::Read;
        let id = reader.read_u16()?;
        let data_len = tag_len - 2;
        let mut jpeg_data = Vec::with_capacity(data_len);
        reader.get_mut().read_to_end(&mut jpeg_data)?;
        let bitmap_info = context.renderer.register_bitmap_jpeg(
            &jpeg_data,
            context
                .library
                .library_for_movie_mut(self.movie())
                .jpeg_tables(),
        )?;
        let bitmap = crate::display_object::Bitmap::new(
            context,
            id,
            bitmap_info.handle,
            bitmap_info.width,
            bitmap_info.height,
        );
        context
            .library
            .library_for_movie_mut(self.movie())
            .register_character(id, Character::Bitmap(bitmap));
        Ok(())
    }

    #[inline]
    fn define_bits_jpeg_2(
        &mut self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        reader: &mut SwfStream<'a>,
        tag_len: usize,
    ) -> DecodeResult {
        use std::io::Read;
        let id = reader.read_u16()?;
        let data_len = tag_len - 2;
        let mut jpeg_data = Vec::with_capacity(data_len);
        reader.get_mut().read_to_end(&mut jpeg_data)?;
        let bitmap_info = context.renderer.register_bitmap_jpeg_2(&jpeg_data)?;
        let bitmap = crate::display_object::Bitmap::new(
            context,
            id,
            bitmap_info.handle,
            bitmap_info.width,
            bitmap_info.height,
        );
        context
            .library
            .library_for_movie_mut(self.movie())
            .register_character(id, Character::Bitmap(bitmap));
        Ok(())
    }

    #[inline]
    fn define_bits_jpeg_3(
        &mut self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        reader: &mut SwfStream<'a>,
        tag_len: usize,
    ) -> DecodeResult {
        use std::io::Read;
        let id = reader.read_u16()?;
        let jpeg_len = reader.read_u32()? as usize;
        let alpha_len = tag_len
            .checked_sub(jpeg_len + 6)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "Invalid jpeg length"))?;
        let mut jpeg_data = Vec::with_capacity(jpeg_len);
        let mut alpha_data = Vec::with_capacity(alpha_len);
        reader
            .get_mut()
            .take(jpeg_len as u64)
            .read_to_end(&mut jpeg_data)?;
        reader
            .get_mut()
            .take(alpha_len as u64)
            .read_to_end(&mut alpha_data)?;
        let bitmap_info = context
            .renderer
            .register_bitmap_jpeg_3(&jpeg_data, &alpha_data)?;
        let bitmap = Bitmap::new(
            context,
            id,
            bitmap_info.handle,
            bitmap_info.width,
            bitmap_info.height,
        );
        context
            .library
            .library_for_movie_mut(self.movie())
            .register_character(id, Character::Bitmap(bitmap));
        Ok(())
    }

    #[inline]
    fn define_bits_jpeg_4(
        &mut self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        reader: &mut SwfStream<'a>,
        tag_len: usize,
    ) -> DecodeResult {
        use std::io::Read;
        let id = reader.read_u16()?;
        let jpeg_len = reader.read_u32()? as usize;
        let _deblocking = reader.read_u16()?;
        let alpha_len = tag_len
            .checked_sub(jpeg_len + 6)
            .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "Invalid jpeg length"))?;
        let mut jpeg_data = Vec::with_capacity(jpeg_len);
        let mut alpha_data = Vec::with_capacity(alpha_len);
        reader
            .get_mut()
            .take(jpeg_len as u64)
            .read_to_end(&mut jpeg_data)?;
        reader
            .get_mut()
            .take(alpha_len as u64)
            .read_to_end(&mut alpha_data)?;
        let bitmap_info = context
            .renderer
            .register_bitmap_jpeg_3(&jpeg_data, &alpha_data)?;
        let bitmap = Bitmap::new(
            context,
            id,
            bitmap_info.handle,
            bitmap_info.width,
            bitmap_info.height,
        );
        context
            .library
            .library_for_movie_mut(self.movie())
            .register_character(id, Character::Bitmap(bitmap));
        Ok(())
    }

    #[inline]
    fn define_button_1(
        &mut self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        reader: &mut SwfStream<'a>,
    ) -> DecodeResult {
        let swf_button = reader.read_define_button_1()?;
        let button = Button::from_swf_tag(
            &swf_button,
            &self.static_data.swf,
            &context.library,
            context.gc_context,
        );
        context
            .library
            .library_for_movie_mut(self.movie())
            .register_character(swf_button.id, Character::Button(button));
        Ok(())
    }

    #[inline]
    fn define_button_2(
        &mut self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        reader: &mut SwfStream<'a>,
    ) -> DecodeResult {
        let swf_button = reader.read_define_button_2()?;
        let button = Button::from_swf_tag(
            &swf_button,
            &self.static_data.swf,
            &context.library,
            context.gc_context,
        );
        context
            .library
            .library_for_movie_mut(self.movie())
            .register_character(swf_button.id, Character::Button(button));
        Ok(())
    }

    #[inline]
    fn define_button_cxform(
        &mut self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        reader: &mut SwfStream<'a>,
        tag_len: usize,
    ) -> DecodeResult {
        let button_colors = reader.read_define_button_cxform(tag_len)?;
        if let Some(button) = context
            .library
            .library_for_movie_mut(self.movie())
            .character_by_id(button_colors.id)
        {
            if let Character::Button(button) = button {
                button.set_colors(context.gc_context, &button_colors.color_transforms[..]);
            } else {
                log::warn!(
                    "DefineButtonCxform: Tried to apply on non-button ID {}",
                    button_colors.id
                );
            }
        } else {
            log::warn!(
                "DefineButtonCxform: Character ID {} doesn't exist",
                button_colors.id
            );
        }
        Ok(())
    }

    #[inline]
    fn define_button_sound(
        &mut self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        reader: &mut SwfStream<'a>,
    ) -> DecodeResult {
        let button_sounds = reader.read_define_button_sound()?;
        if let Some(button) = context
            .library
            .library_for_movie_mut(self.movie())
            .character_by_id(button_sounds.id)
        {
            if let Character::Button(button) = button {
                button.set_sounds(context.gc_context, button_sounds);
            } else {
                log::warn!(
                    "DefineButtonSound: Tried to apply on non-button ID {}",
                    button_sounds.id
                );
            }
        } else {
            log::warn!(
                "DefineButtonSound: Character ID {} doesn't exist",
                button_sounds.id
            );
        }
        Ok(())
    }

    /// Defines a dynamic text field character.
    #[inline]
    fn define_edit_text(
        &mut self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        reader: &mut SwfStream<'a>,
    ) -> DecodeResult {
        let swf_edit_text = reader.read_define_edit_text()?;
        let edit_text = EditText::from_swf_tag(context, self.movie(), swf_edit_text);
        context
            .library
            .library_for_movie_mut(self.movie())
            .register_character(edit_text.id(), Character::EditText(edit_text));
        Ok(())
    }

    #[inline]
    fn define_font_1(
        &mut self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        reader: &mut SwfStream<'a>,
    ) -> DecodeResult {
        let font = reader.read_define_font_1()?;
        let glyphs = font
            .glyphs
            .into_iter()
            .map(|g| swf::Glyph {
                shape_records: g,
                code: 0,
                advance: None,
                bounds: None,
            })
            .collect::<Vec<_>>();

        let font = swf::Font {
            id: font.id,
            version: 0,
            name: "".into(),
            glyphs,
            language: swf::Language::Unknown,
            layout: None,
            is_small_text: false,
            is_shift_jis: false,
            is_ansi: false,
            is_bold: false,
            is_italic: false,
        };
        let font_object = Font::from_swf_tag(
            context.gc_context,
            context.renderer,
            &font,
            reader.encoding(),
        )
        .unwrap();
        context
            .library
            .library_for_movie_mut(self.movie())
            .register_character(font.id, Character::Font(font_object));
        Ok(())
    }

    #[inline]
    fn define_font_2(
        &mut self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        reader: &mut SwfStream<'a>,
    ) -> DecodeResult {
        let font = reader.read_define_font_2(2)?;
        let font_object = Font::from_swf_tag(
            context.gc_context,
            context.renderer,
            &font,
            reader.encoding(),
        )
        .unwrap();
        context
            .library
            .library_for_movie_mut(self.movie())
            .register_character(font.id, Character::Font(font_object));
        Ok(())
    }

    #[inline]
    fn define_font_3(
        &mut self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        reader: &mut SwfStream<'a>,
    ) -> DecodeResult {
        let font = reader.read_define_font_2(3)?;
        let font_object = Font::from_swf_tag(
            context.gc_context,
            context.renderer,
            &font,
            reader.encoding(),
        )
        .unwrap();
        context
            .library
            .library_for_movie_mut(self.movie())
            .register_character(font.id, Character::Font(font_object));

        Ok(())
    }

    #[inline]
    fn define_font_4(
        &mut self,
        _context: &mut UpdateContext<'_, 'gc, '_>,
        _reader: &mut SwfStream<'a>,
    ) -> DecodeResult {
        log::warn!("DefineFont4 tag (TLF text) is not implemented");
        Ok(())
    }

    #[inline]
    fn define_sound(
        &mut self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        reader: &mut SwfStream<'a>,
    ) -> DecodeResult {
        let sound = reader.read_define_sound()?;
        if let Ok(handle) = context.audio.register_sound(&sound) {
            context
                .library
                .library_for_movie_mut(self.movie())
                .register_character(sound.id, Character::Sound(handle));
        } else {
            log::error!(
                "MovieClip::define_sound: Unable to register sound ID {}",
                sound.id
            );
        }
        Ok(())
    }

    #[inline]
    fn define_video_stream(
        &mut self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        reader: &mut SwfStream,
    ) -> DecodeResult {
        match reader.read_define_video_stream()? {
            Tag::DefineVideoStream(streamdef) => {
                let id = streamdef.id;
                let video = Video::from_swf_tag(self.movie(), streamdef, context.gc_context);
                context
                    .library
                    .library_for_movie_mut(self.movie())
                    .register_character(id, Character::Video(video));
            }
            _ => unreachable!(),
        };

        Ok(())
    }

    fn define_sprite(
        &mut self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        reader: &mut SwfStream<'a>,
        tag_len: usize,
        morph_shapes: &mut fnv::FnvHashMap<CharacterId, MorphShapeStatic>,
    ) -> DecodeResult {
        let id = reader.read_character_id()?;
        let num_frames = reader.read_u16()?;
        let movie_clip = MovieClip::new_with_data(
            context.gc_context,
            id,
            self.static_data
                .swf
                .resize_to_reader(reader, tag_len - 4)
                .ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::Other,
                        "Cannot define sprite with invalid offset and length!",
                    )
                })?,
            num_frames,
        );

        movie_clip.preload(context, morph_shapes);

        context
            .library
            .library_for_movie_mut(self.movie())
            .register_character(id, Character::MovieClip(movie_clip));

        Ok(())
    }

    #[inline]
    fn define_text(
        &mut self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        reader: &mut SwfStream<'a>,
        version: u8,
    ) -> DecodeResult {
        let text = reader.read_define_text(version)?;
        let text_object = Text::from_swf_tag(context, self.movie(), &text);
        context
            .library
            .library_for_movie_mut(self.movie())
            .register_character(text.id, Character::Text(text_object));
        Ok(())
    }

    #[inline]
    fn script_limits(&mut self, reader: &mut SwfStream<'a>, avm: &mut Avm1<'gc>) -> DecodeResult {
        let max_recursion_depth = reader.read_u16()?;
        let _timeout_in_seconds = reader.read_u16()?;

        avm.set_max_recursion_depth(max_recursion_depth);

        Ok(())
    }

    #[inline]
    fn export_assets(
        &mut self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        reader: &mut SwfStream<'a>,
    ) -> DecodeResult {
        let exports = reader.read_export_assets()?;
        for export in exports {
            let name = export.name.to_str_lossy(reader.encoding());
            let character = context
                .library
                .library_for_movie_mut(self.movie())
                .register_export(export.id, &name);

            // TODO: do other types of Character need to know their exported name?
            if let Some(Character::MovieClip(movie_clip)) = character {
                *movie_clip.0.read().static_data.exported_name.borrow_mut() =
                    Some(name.to_string());
            }
        }
        Ok(())
    }

    #[inline]
    fn frame_label(
        &mut self,
        _context: &mut UpdateContext<'_, 'gc, '_>,
        reader: &mut SwfStream<'a>,
        tag_len: usize,
        cur_frame: FrameNumber,
        static_data: &mut MovieClipStatic,
    ) -> DecodeResult {
        let frame_label = reader.read_frame_label(tag_len)?;
        // Frame labels are case insensitive (ASCII).
        let label = frame_label
            .label
            .to_str_lossy(reader.encoding())
            .to_ascii_lowercase();
        if let std::collections::hash_map::Entry::Vacant(v) = static_data.frame_labels.entry(label)
        {
            v.insert(cur_frame);
        } else {
            log::warn!("Movie clip {}: Duplicated frame label", self.id());
        }
        Ok(())
    }

    #[inline]
    fn jpeg_tables(
        &mut self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        reader: &mut SwfStream<'a>,
        tag_len: usize,
    ) -> DecodeResult {
        use std::io::Read;
        // TODO(Herschel): Can we use a slice instead of copying?
        let mut jpeg_data = Vec::with_capacity(tag_len);
        reader.get_mut().read_to_end(&mut jpeg_data)?;
        context
            .library
            .library_for_movie_mut(self.movie())
            .set_jpeg_tables(jpeg_data);
        Ok(())
    }

    #[inline]
    fn preload_remove_object(
        &mut self,
        _context: &mut UpdateContext<'_, 'gc, '_>,
        reader: &mut SwfStream<'a>,
        ids: &mut fnv::FnvHashMap<Depth, CharacterId>,
        version: u8,
    ) -> DecodeResult {
        let remove_object = if version == 1 {
            reader.read_remove_object_1()
        } else {
            reader.read_remove_object_2()
        }?;
        ids.remove(&remove_object.depth.into());
        Ok(())
    }

    #[inline]
    fn preload_show_frame(
        &mut self,
        _context: &mut UpdateContext<'_, 'gc, '_>,
        _reader: &mut SwfStream<'a>,
        cur_frame: &mut FrameNumber,
    ) -> DecodeResult {
        *cur_frame += 1;
        Ok(())
    }
}

// Control tags
impl<'gc, 'a> MovieClip<'gc> {
    #[inline]
    fn do_action(
        self,
        self_display_object: DisplayObject<'gc>,
        context: &mut UpdateContext<'_, 'gc, '_>,
        reader: &mut SwfStream<'a>,
        tag_len: usize,
    ) -> DecodeResult {
        let movie = self.movie().unwrap();
        let library = context.library.library_for_movie_mut(movie);
        if let Err(e) = library.check_avm_type(AvmType::Avm1) {
            log::warn!("{}", e);

            return Ok(());
        }

        // Queue the actions.
        let slice = self
            .0
            .read()
            .static_data
            .swf
            .resize_to_reader(reader, tag_len)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "Invalid source or tag length when running action",
                )
            })?;
        context.action_queue.queue_actions(
            self_display_object,
            ActionType::Normal { bytecode: slice },
            false,
        );
        Ok(())
    }

    fn place_object(
        self,
        self_display_object: DisplayObject<'gc>,
        context: &mut UpdateContext<'_, 'gc, '_>,
        reader: &mut SwfStream<'a>,
        tag_len: usize,
        version: u8,
    ) -> DecodeResult {
        let place_object = if version == 1 {
            reader.read_place_object(tag_len)
        } else {
            reader.read_place_object_2_or_3(version)
        }?;
        use swf::PlaceObjectAction;
        match place_object.action {
            PlaceObjectAction::Place(id) | PlaceObjectAction::Replace(id) => {
                if let Some(child) = self.instantiate_child(
                    self_display_object,
                    context,
                    id,
                    place_object.depth.into(),
                    &place_object,
                    matches!(place_object.action, PlaceObjectAction::Replace(_)),
                ) {
                    child
                } else {
                    return Ok(());
                }
            }
            PlaceObjectAction::Modify => {
                if let Some(child) = self.child_by_depth(place_object.depth.into()) {
                    child.apply_place_object(context, self.movie(), &place_object);
                    child
                } else {
                    return Ok(());
                }
            }
        };

        Ok(())
    }

    #[inline]
    fn remove_object(
        mut self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        reader: &mut SwfStream<'a>,
        version: u8,
    ) -> DecodeResult {
        let remove_object = if version == 1 {
            reader.read_remove_object_1()
        } else {
            reader.read_remove_object_2()
        }?;

        if let Some(child) = self.child_by_depth(remove_object.depth.into()) {
            if !child.placed_by_script() {
                self.remove_child(context, child, Lists::all());
            } else {
                self.remove_child(context, child, Lists::DEPTH);
            }
        }

        Ok(())
    }

    #[inline]
    fn set_background_color(
        self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        reader: &mut SwfStream<'a>,
    ) -> DecodeResult {
        // Set background color if none set
        // bgcolor attribute on the HTML embed would override this
        // Also note that a loaded child SWF could change background color only
        // if parent SWF is missing SetBackgroundColor tag.
        let background_color = reader.read_rgb()?;
        if context.background_color.is_none() {
            *context.background_color = Some(background_color);
        }
        Ok(())
    }

    #[inline]
    fn sound_stream_block(
        self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        _reader: &mut SwfStream<'a>,
    ) -> DecodeResult {
        let mc = self.0.read();
        if mc.playing() {
            if let (Some(stream_info), None) = (&mc.static_data.audio_stream_info, mc.audio_stream)
            {
                let slice = mc
                    .static_data
                    .swf
                    .to_start_and_end(mc.tag_stream_pos as usize, mc.tag_stream_len())
                    .ok_or_else(|| {
                        std::io::Error::new(
                            std::io::ErrorKind::Other,
                            "Invalid slice generated when constructing sound stream block",
                        )
                    })?;
                let audio_stream = context.start_stream(
                    mc.static_data.audio_stream_handle,
                    self,
                    mc.current_frame() + 1,
                    slice,
                    &stream_info,
                );
                drop(mc);
                self.0.write(context.gc_context).audio_stream = audio_stream;
            }
        }

        Ok(())
    }

    #[inline]
    fn start_sound_1(
        self,
        context: &mut UpdateContext<'_, 'gc, '_>,
        reader: &mut SwfStream<'a>,
    ) -> DecodeResult {
        let start_sound = reader.read_start_sound_1()?;
        if let Some(handle) = context
            .library
            .library_for_movie_mut(self.movie().unwrap()) // TODO
            .get_sound(start_sound.id)
        {
            use swf::SoundEvent;
            // The sound event type is controlled by the "Sync" setting in the Flash IDE.
            match start_sound.sound_info.event {
                // "Event" sounds always play, independent of the timeline.
                SoundEvent::Event => {
                    let _ = context.start_sound(
                        handle,
                        &start_sound.sound_info,
                        Some(self.into()),
                        None,
                    );
                }

                // "Start" sounds only play if an instance of the same sound is not already playing.
                SoundEvent::Start => {
                    if !context.is_sound_playing_with_handle(handle) {
                        let _ = context.start_sound(
                            handle,
                            &start_sound.sound_info,
                            Some(self.into()),
                            None,
                        );
                    }
                }

                // "Stop" stops any active instances of a given sound.
                SoundEvent::Stop => context.stop_sounds_with_handle(handle),
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct Scene {
    pub name: String,
    pub start: FrameNumber,
    pub length: FrameNumber,
}

impl Default for Scene {
    fn default() -> Self {
        Scene {
            name: "".to_string(),
            start: 0,
            length: u16::MAX,
        }
    }
}

/// Static data shared between all instances of a movie clip.
#[allow(dead_code)]
#[derive(Clone, Collect)]
#[collect(require_static)]
struct MovieClipStatic {
    id: CharacterId,
    swf: SwfSlice,
    frame_labels: HashMap<String, FrameNumber>,
    scene_labels: HashMap<String, Scene>,
    audio_stream_info: Option<swf::SoundStreamHead>,
    audio_stream_handle: Option<SoundHandle>,
    total_frames: FrameNumber,
    /// The last known symbol name under which this movie clip was exported.
    /// Used for looking up constructors registered with `Object.registerClass`.
    exported_name: RefCell<Option<String>>,
}

impl MovieClipStatic {
    fn empty(swf: SwfSlice) -> Self {
        Self::with_data(0, swf, 1)
    }

    fn with_data(id: CharacterId, swf: SwfSlice, total_frames: FrameNumber) -> Self {
        Self {
            id,
            swf,
            total_frames,
            frame_labels: HashMap::new(),
            scene_labels: HashMap::new(),
            audio_stream_info: None,
            audio_stream_handle: None,
            exported_name: RefCell::new(None),
        }
    }
}

/// Stores the placement settings for display objects during a
/// goto command.
#[derive(Debug)]
struct GotoPlaceObject<'a> {
    /// The frame number that this character was first placed on.
    frame: FrameNumber,
    /// The display properties of the object.
    place_object: swf::PlaceObject<'a>,
    /// Increasing index of this place command, for sorting.
    index: usize,
}

impl<'a> GotoPlaceObject<'a> {
    fn new(
        frame: FrameNumber,
        mut place_object: swf::PlaceObject<'a>,
        is_rewind: bool,
        index: usize,
    ) -> Self {
        if is_rewind {
            if let swf::PlaceObjectAction::Place(_) = place_object.action {
                if place_object.matrix.is_none() {
                    place_object.matrix = Some(Default::default());
                }
                if place_object.color_transform.is_none() {
                    place_object.color_transform = Some(Default::default());
                }
                if place_object.ratio.is_none() {
                    place_object.ratio = Some(Default::default());
                }
                if place_object.name.is_none() {
                    place_object.name = Some(Default::default());
                }
                if place_object.clip_depth.is_none() {
                    place_object.clip_depth = Some(Default::default());
                }
                if place_object.class_name.is_none() {
                    place_object.class_name = Some(Default::default());
                }
            }
        }

        Self {
            frame,
            place_object,
            index,
        }
    }

    #[inline]
    fn id(&self) -> CharacterId {
        match &self.place_object.action {
            swf::PlaceObjectAction::Place(id) | swf::PlaceObjectAction::Replace(id) => *id,
            swf::PlaceObjectAction::Modify => 0,
        }
    }

    #[inline]
    fn modifies_original_item(&self) -> bool {
        matches!(
            &self.place_object.action,
            swf::PlaceObjectAction::Replace(_)
        )
    }

    #[inline]
    fn depth(&self) -> Depth {
        self.place_object.depth.into()
    }

    fn merge(&mut self, next: &mut GotoPlaceObject<'a>) {
        use swf::PlaceObjectAction;
        let cur_place = &mut self.place_object;
        let next_place = &mut next.place_object;
        match (cur_place.action, next_place.action) {
            (cur, PlaceObjectAction::Modify) => {
                cur_place.action = cur;
            }
            (_, new) => {
                cur_place.action = new;
                self.frame = next.frame;
            }
        };
        if next_place.matrix.is_some() {
            cur_place.matrix = next_place.matrix.take();
        }
        if next_place.color_transform.is_some() {
            cur_place.color_transform = next_place.color_transform.take();
        }
        if next_place.ratio.is_some() {
            cur_place.ratio = next_place.ratio.take();
        }
        if next_place.name.is_some() {
            cur_place.name = next_place.name.take();
        }
        if next_place.clip_depth.is_some() {
            cur_place.clip_depth = next_place.clip_depth.take();
        }
        if next_place.class_name.is_some() {
            cur_place.class_name = next_place.class_name.take();
        }
        if next_place.background_color.is_some() {
            cur_place.background_color = next_place.background_color.take();
        }
        // TODO: Other stuff.
    }
}

bitflags! {
    /// Boolean state flags used by `MovieClip`.
    #[derive(Collect)]
    #[collect(require_static)]
    struct MovieClipFlags: u8 {
        /// Whether this `MovieClip` has run its initial frame.
        const INITIALIZED             = 1 << 0;

        /// Whether this `MovieClip` is playing or stopped.
        const PLAYING                 = 1 << 1;

        /// Whether this `MovieClip` has been played as a result of an AS3 command.
        ///
        /// The AS3 `isPlaying` property is broken and yields false until you first
        /// call `play` to unbreak it. This flag tracks that bug.
        const PROGRAMMATICALLY_PLAYED = 1 << 2;
    }
}

/// Actions that are attached to a `MovieClip` event in
/// an `onClipEvent`/`on` handler.
#[derive(Debug, Clone, Collect)]
#[collect(require_static)]
pub struct ClipAction {
    /// The event that triggers this handler.
    event: ClipEvent,

    /// The actions to run.
    action_data: SwfSlice,
}

impl ClipAction {
    /// Build a set of clip actions from a SWF movie and a parsed ClipAction.
    ///
    /// TODO: Our underlying SWF parser currently does not yield slices of the
    /// underlying movie, so we cannot convert those slices into a `SwfSlice`.
    /// Instead, we have to construct a fake `SwfMovie` just to hold one clip
    /// action.
    pub fn from_action_and_movie(
        other: swf::ClipAction<'_>,
        movie: Arc<SwfMovie>,
    ) -> impl Iterator<Item = Self> {
        use swf::ClipEventFlag;

        let key_code = other.key_code;
        let action_data = SwfSlice::from(movie)
            .to_unbounded_subslice(other.action_data)
            .unwrap();

        let mut events = Vec::new();
        let bits = other.events.bits();
        let mut bit = 1u32;
        while bits & !(bit - 1) != 0 {
            if bits & bit != 0 {
                events.push(ClipEventFlag::from_bits_truncate(bit));
            }
            bit <<= 1;
        }
        events.into_iter().map(move |event| Self {
            event: match event {
                ClipEventFlag::CONSTRUCT => ClipEvent::Construct,
                ClipEventFlag::DATA => ClipEvent::Data,
                ClipEventFlag::DRAG_OUT => ClipEvent::DragOut,
                ClipEventFlag::DRAG_OVER => ClipEvent::DragOver,
                ClipEventFlag::ENTER_FRAME => ClipEvent::EnterFrame,
                ClipEventFlag::INITIALIZE => ClipEvent::Initialize,
                ClipEventFlag::KEY_UP => ClipEvent::KeyUp,
                ClipEventFlag::KEY_DOWN => ClipEvent::KeyDown,
                ClipEventFlag::KEY_PRESS => ClipEvent::KeyPress {
                    key_code: key_code
                        .and_then(|k| ButtonKeyCode::try_from(k).ok())
                        .unwrap_or(ButtonKeyCode::Unknown),
                },
                ClipEventFlag::LOAD => ClipEvent::Load,
                ClipEventFlag::MOUSE_UP => ClipEvent::MouseUp,
                ClipEventFlag::MOUSE_DOWN => ClipEvent::MouseDown,
                ClipEventFlag::MOUSE_MOVE => ClipEvent::MouseMove,
                ClipEventFlag::PRESS => ClipEvent::Press,
                ClipEventFlag::ROLL_OUT => ClipEvent::RollOut,
                ClipEventFlag::ROLL_OVER => ClipEvent::RollOver,
                ClipEventFlag::RELEASE => ClipEvent::Release,
                ClipEventFlag::RELEASE_OUTSIDE => ClipEvent::ReleaseOutside,
                ClipEventFlag::UNLOAD => ClipEvent::Unload,
                _ => unreachable!(),
            },
            action_data: action_data.clone(),
        })
    }
}

/// An AVM2 frame script attached to a (presumably AVM2) MovieClip.
#[derive(Debug, Clone, Collect)]
#[collect(no_drop)]
pub struct Avm2FrameScript<'gc> {
    /// The frame to invoke this frame script on.
    pub frame_id: FrameNumber,

    /// The AVM2 callable object to invoke when the frame script runs.
    pub callable: Avm2Object<'gc>,
}

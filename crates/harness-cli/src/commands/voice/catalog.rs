//! Voice catalog across Antigravity/Gemini, OpenAI, and Edge TTS.

/// Catalog entry for a built-in TTS voice across Antigravity/Gemini, OpenAI, and Edge TTS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoiceCatalogEntry {
    pub id: &'static str,
    pub provider: &'static str,
    pub provider_label: &'static str,
    pub description: &'static str,
}

#[allow(dead_code)]
pub fn is_openai_voice(voice: &str) -> bool {
    matches!(
        voice.trim(),
        "alloy"
            | "ash"
            | "ballad"
            | "coral"
            | "echo"
            | "fable"
            | "nova"
            | "onyx"
            | "sage"
            | "shimmer"
            | "verse"
            | "marin"
            | "cedar"
    )
}

/// Return all available built-in voices across Gemini/Antigravity, OpenAI, and Edge TTS.
pub fn available_voices() -> Vec<VoiceCatalogEntry> {
    vec![
        // Antigravity / Gemini Prebuilt Voices (30)
        VoiceCatalogEntry {
            id: "Aoede",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Breezy (default)",
        },
        VoiceCatalogEntry {
            id: "Zephyr",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Bright",
        },
        VoiceCatalogEntry {
            id: "Puck",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Upbeat",
        },
        VoiceCatalogEntry {
            id: "Charon",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Informative",
        },
        VoiceCatalogEntry {
            id: "Kore",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Firm",
        },
        VoiceCatalogEntry {
            id: "Fenrir",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Excited",
        },
        VoiceCatalogEntry {
            id: "Leda",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Youthful",
        },
        VoiceCatalogEntry {
            id: "Orus",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Firm",
        },
        VoiceCatalogEntry {
            id: "Callirrhoe",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Easygoing",
        },
        VoiceCatalogEntry {
            id: "Autonoe",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Bright",
        },
        VoiceCatalogEntry {
            id: "Thyone",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Engaged",
        },
        VoiceCatalogEntry {
            id: "Iapetus",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Clear",
        },
        VoiceCatalogEntry {
            id: "Enceladus",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Breathy",
        },
        VoiceCatalogEntry {
            id: "Pandora",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Lively",
        },
        VoiceCatalogEntry {
            id: "Helike",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Easygoing",
        },
        VoiceCatalogEntry {
            id: "Ersa",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Calm",
        },
        VoiceCatalogEntry {
            id: "Polydeuces",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Dynamic",
        },
        VoiceCatalogEntry {
            id: "Despina",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Smooth",
        },
        VoiceCatalogEntry {
            id: "Carme",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Engaged",
        },
        VoiceCatalogEntry {
            id: "Arche",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Approachable",
        },
        VoiceCatalogEntry {
            id: "Aegaeon",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Forward",
        },
        VoiceCatalogEntry {
            id: "Alcmene",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Warm",
        },
        VoiceCatalogEntry {
            id: "Harpalyke",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Approachable",
        },
        VoiceCatalogEntry {
            id: "Kalyke",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Gentle",
        },
        VoiceCatalogEntry {
            id: "Lysithea",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Calm",
        },
        VoiceCatalogEntry {
            id: "Metis",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Expressive",
        },
        VoiceCatalogEntry {
            id: "Mneme",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Warm",
        },
        VoiceCatalogEntry {
            id: "Praxidike",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Empathetic",
        },
        VoiceCatalogEntry {
            id: "Thelxinoe",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Careful",
        },
        VoiceCatalogEntry {
            id: "Sulafat",
            provider: "antigravity",
            provider_label: "Gemini / Agy",
            description: "Gentle",
        },
        // OpenAI Built-in Voices (13)
        VoiceCatalogEntry {
            id: "alloy",
            provider: "openai",
            provider_label: "OpenAI",
            description: "Neutral & balanced",
        },
        VoiceCatalogEntry {
            id: "ash",
            provider: "openai",
            provider_label: "OpenAI",
            description: "Clear & casual",
        },
        VoiceCatalogEntry {
            id: "ballad",
            provider: "openai",
            provider_label: "OpenAI",
            description: "Melodic & warm",
        },
        VoiceCatalogEntry {
            id: "coral",
            provider: "openai",
            provider_label: "OpenAI",
            description: "Bright & friendly",
        },
        VoiceCatalogEntry {
            id: "echo",
            provider: "openai",
            provider_label: "OpenAI",
            description: "Smooth & authoritative",
        },
        VoiceCatalogEntry {
            id: "fable",
            provider: "openai",
            provider_label: "OpenAI",
            description: "Expressive & storybook",
        },
        VoiceCatalogEntry {
            id: "nova",
            provider: "openai",
            provider_label: "OpenAI",
            description: "Energetic & warm",
        },
        VoiceCatalogEntry {
            id: "onyx",
            provider: "openai",
            provider_label: "OpenAI",
            description: "Deep & authoritative",
        },
        VoiceCatalogEntry {
            id: "sage",
            provider: "openai",
            provider_label: "OpenAI",
            description: "Insightful & calm",
        },
        VoiceCatalogEntry {
            id: "shimmer",
            provider: "openai",
            provider_label: "OpenAI",
            description: "Clear & upbeat",
        },
        VoiceCatalogEntry {
            id: "verse",
            provider: "openai",
            provider_label: "OpenAI",
            description: "Versatile & relaxed",
        },
        VoiceCatalogEntry {
            id: "marin",
            provider: "openai",
            provider_label: "OpenAI",
            description: "Natural & conversational",
        },
        VoiceCatalogEntry {
            id: "cedar",
            provider: "openai",
            provider_label: "OpenAI",
            description: "Grounded & articulate",
        },
        // Microsoft Edge Free High-Definition Neural Voices (30)
        VoiceCatalogEntry {
            id: "en-US-EmmaMultilingualNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "US Female · Cheerful & expressive",
        },
        VoiceCatalogEntry {
            id: "en-US-BrianMultilingualNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "US Male · Natural & balanced",
        },
        VoiceCatalogEntry {
            id: "en-US-AvaMultilingualNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "US Female · Warm & friendly",
        },
        VoiceCatalogEntry {
            id: "en-US-AndrewMultilingualNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "US Male · Confident & warm",
        },
        VoiceCatalogEntry {
            id: "en-US-JennyNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "US Female · Natural conversational",
        },
        VoiceCatalogEntry {
            id: "en-US-GuyNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "US Male · Natural conversational",
        },
        VoiceCatalogEntry {
            id: "en-US-AriaNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "US Female · Confident & crisp",
        },
        VoiceCatalogEntry {
            id: "en-US-DavisNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "US Male · Friendly & engaging",
        },
        VoiceCatalogEntry {
            id: "en-US-JaneNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "US Female · Warm & gentle",
        },
        VoiceCatalogEntry {
            id: "en-US-JasonNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "US Male · Casual & upbeat",
        },
        VoiceCatalogEntry {
            id: "en-US-SaraNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "US Female · Cheerful & bright",
        },
        VoiceCatalogEntry {
            id: "en-US-TonyNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "US Male · Deep & professional",
        },
        VoiceCatalogEntry {
            id: "en-US-NancyNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "US Female · Friendly & lively",
        },
        VoiceCatalogEntry {
            id: "en-US-AmberNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "US Female · Warm & expressive",
        },
        VoiceCatalogEntry {
            id: "en-US-ChristopherNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "US Male · Calm & clear",
        },
        VoiceCatalogEntry {
            id: "en-US-EricNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "US Male · Rational & crisp",
        },
        VoiceCatalogEntry {
            id: "en-US-MichelleNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "US Female · Pleasant & articulate",
        },
        VoiceCatalogEntry {
            id: "en-US-RogerNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "US Male · Lively",
        },
        VoiceCatalogEntry {
            id: "en-US-SteffanNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "US Male · Mature",
        },
        VoiceCatalogEntry {
            id: "en-US-AnaNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "US Female · Cute",
        },
        VoiceCatalogEntry {
            id: "en-GB-SoniaNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "UK Female · Friendly",
        },
        VoiceCatalogEntry {
            id: "en-GB-RyanNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "UK Male · Friendly",
        },
        VoiceCatalogEntry {
            id: "en-GB-LibbyNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "UK Female · Bright",
        },
        VoiceCatalogEntry {
            id: "en-GB-MaisieNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "UK Female · Youthful",
        },
        VoiceCatalogEntry {
            id: "en-GB-ThomasNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "UK Male · Calm",
        },
        VoiceCatalogEntry {
            id: "en-AU-NatashaNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "AU Female · Friendly",
        },
        VoiceCatalogEntry {
            id: "en-AU-WilliamMultilingualNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "AU Male · Friendly",
        },
        VoiceCatalogEntry {
            id: "en-CA-ClaraNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "CA Female · Friendly",
        },
        VoiceCatalogEntry {
            id: "en-CA-LiamNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "CA Male · Friendly",
        },
        VoiceCatalogEntry {
            id: "en-IE-EmilyNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "IE Female · Friendly",
        },
        VoiceCatalogEntry {
            id: "en-IE-ConnorNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "IE Male · Friendly",
        },
        VoiceCatalogEntry {
            id: "en-IN-NeerjaExpressiveNeural",
            provider: "groq",
            provider_label: "Edge TTS",
            description: "IN Female · Expressive",
        },
    ]
}

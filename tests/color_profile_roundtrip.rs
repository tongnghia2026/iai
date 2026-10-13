use iai::core::cms::{self, WorkingProfile, DEFAULT_INTENT};

#[test]
fn built_in_rgb_profiles_roundtrip_rgba_and_preserve_alpha() {
    let original = vec![
        0, 0, 0, 0, 18, 64, 220, 17, 128, 128, 128, 128, 245, 80, 30, 255, 255, 255, 255, 201,
    ];
    for profile in WorkingProfile::all() {
        let mut converted = original.clone();
        let dst = profile.profile();
        assert!(cms::convert_rgba8(
            &mut converted,
            &cms::srgb_profile(),
            &dst,
            DEFAULT_INTENT
        ));
        assert!(cms::convert_rgba8(
            &mut converted,
            &dst,
            &cms::srgb_profile(),
            DEFAULT_INTENT
        ));
        for pixel in 0..original.len() / 4 {
            assert_eq!(converted[pixel * 4 + 3], original[pixel * 4 + 3]);
            for channel in 0..3 {
                assert!(
                    converted[pixel * 4 + channel].abs_diff(original[pixel * 4 + channel]) <= 6,
                    "{} roundtrip drift at pixel {pixel}: {:?} -> {:?}",
                    profile.name(),
                    &original[pixel * 4..pixel * 4 + 4],
                    &converted[pixel * 4..pixel * 4 + 4]
                );
            }
        }
    }
}

#[test]
fn wide_gamut_roundtrip_keeps_16bit_precision_and_alpha() {
    let original = vec![
        0u16, 0, 0, 1, 16_000, 24_000, 48_000, 12_345, 32_768, 32_768, 32_768, 65_535, 55_000,
        24_000, 16_000, 50_000,
    ];
    for profile in WorkingProfile::all() {
        let mut converted = original.clone();
        let destination = profile.profile();
        assert!(cms::convert_rgba16(
            &mut converted,
            &cms::srgb_profile(),
            &destination,
            DEFAULT_INTENT
        ));
        assert!(cms::convert_rgba16(
            &mut converted,
            &destination,
            &cms::srgb_profile(),
            DEFAULT_INTENT
        ));
        for pixel in 0..original.len() / 4 {
            assert_eq!(converted[pixel * 4 + 3], original[pixel * 4 + 3]);
            for channel in 0..3 {
                assert!(
                    converted[pixel * 4 + channel].abs_diff(original[pixel * 4 + channel]) <= 512,
                    "{} 16-bit drift: {:?} -> {:?}",
                    profile.name(),
                    &original[pixel * 4..pixel * 4 + 4],
                    &converted[pixel * 4..pixel * 4 + 4]
                );
            }
        }
    }
}

#[test]
fn all_built_in_profiles_serialize_and_reopen_as_rgb() {
    for profile in WorkingProfile::all() {
        let bytes = cms::icc_bytes(&profile.profile());
        assert!(!bytes.is_empty(), "{} did not serialize", profile.name());
        assert!(
            cms::profile_is_rgb(&bytes),
            "{} reopened non-RGB",
            profile.name()
        );
    }
}

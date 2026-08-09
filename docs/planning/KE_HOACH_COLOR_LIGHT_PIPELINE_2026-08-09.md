# Kế hoạch nâng cấp Color Mixer và Light pipeline của IAI

Ngày lập: 2026-08-09  
Trạng thái: Chưa triển khai  
Phạm vi: Develop/RAW, Color Mixer, Light, tone mapping, gamut mapping, preview GPU và color management  
Nguồn tham khảo chính: ART tại `C:\Users\Admin\Pictures\1111\ART`

## 1. Mục tiêu

Nâng chất lượng xử lý ánh sáng và màu của IAI để:

- Thao tác Color Mixer đúng vùng màu, chuyển tiếp mượt và có phản hồi dễ dự đoán.
- Giữ hue và độ sâu màu tốt khi tăng saturation, luminance, highlights và exposure.
- Hạn chế da đỏ/cam gắt, highlight trắng xám, blue/cyan vỡ gamut và màu bị bệt.
- Preview khi kéo, preview sau khi thả chuột và ảnh commit/export phải đồng nhất.
- Hỗ trợ đúng input profile, working profile, monitor/output profile và ảnh wide-gamut.
- Có test định lượng để việc tuning không phụ thuộc hoàn toàn vào cảm giác bằng mắt.

Mục tiêu không phải sao chép nguyên code ART hay Photoshop. IAI sẽ học kiến trúc và nguyên tắc xử lý tốt từ ART, sau đó triển khai phù hợp với pipeline Rust/WGPU hiện có.

## 2. Nguyên tắc bắt buộc

1. Không tuning hằng số khi chưa có ảnh chuẩn và phép đo baseline.
2. Không clamp RGB sau từng bước nếu chưa đến output boundary.
3. Không dùng sRGB làm working space mặc định cho RAW.
4. Phân loại màu và chỉnh màu phải dùng một mô hình nhất quán.
5. Lightness, chroma và hue phải được tách rõ; không gọi RGB gain là perceptual lightness.
6. CPU và GPU phải dùng cùng công thức hoặc cùng LUT được sinh từ một nguồn.
7. Preview nhanh được phép giảm chất lượng trong lúc kéo, nhưng phải tự render full-quality ngay khi thả.
8. Mỗi phase chỉ hoàn thành khi đạt tiêu chí nghiệm thu và có test chống regression.
9. Thay đổi format `.iai` phải dùng `serde(default)` và giữ khả năng mở tài liệu cũ.
10. Không xóa pipeline cũ trước khi pipeline mới vượt qua golden tests và manual review.
11. Giữ nguyên giao diện và workflow Develop hiện tại theo phong cách Photoshop (PTS); không redesign bố cục, panel, tab hoặc cách sử dụng quen thuộc.
12. Thay đổi thuật toán phía sau không được làm người dùng phải học lại thao tác Color Mixer/Light.

## 3. Hiện trạng đã xác định

### 3.1 IAI

- `src/core/develop_scene.rs` giữ scene-linear master nhưng hiện mô tả working RGB bằng sRGB primaries.
- `src/core/develop/color.rs` chỉnh saturation bằng cách scale linear RGB chroma quanh luminance.
- `src/core/develop/mixer.rs` phân loại hue bằng UCS, saturation confidence bằng HSV, xoay hue trong Oklab và chỉnh luminance bằng RGB gain.
- `src/core/develop/mod.rs` dùng color proxy downsample 6, guided filter và chỉ giữ một phần chroma detail.
- `src/core/develop_scene.rs` có highlight chroma compression và gamut mapping kéo chroma về luminance.
- `src/gpu/compositor.wgsl` có bản sao thủ công của nhiều công thức CPU, tạo nguy cơ lệch khi sửa một phía.

### 3.2 ART

- Pipeline có thứ tự stage rõ trong `rtengine/improcfun.cc`.
- Channel Mixer chạy bằng ma trận 3x3 trong working RGB profile tại `rtengine/ipchmixer.cc`.
- Color/Tone Correction có các mode YUV, HSL, RGB và JzAzBz tại `rtengine/ipcolorcorrection.cc`.
- Tone Curve có các mode Standard, Weighted Standard, Film-like, Saturation/Value, Luminance, Perceptual và Neutral tại `rtengine/iptonecurve.cc`.
- ART tách luminance khỏi chroma, dùng working-profile matrix và hạn chế clip sớm.

## 4. Kiến trúc đích

Pipeline đích đề xuất:

```text
Decode RAW / đọc raster + input ICC/DCP
    -> chuyển một lần sang scene-linear wide-gamut working RGB
    -> white balance / CAT
    -> exposure (2^EV)
    -> scene tone equalizer (log luminance, edge-aware)
    -> scene-to-display tone mapping
       [Perceptual | Film-like | Neutral]
    -> color mixer trong perceptual cylindrical space
       [Hue | Chroma/Saturation | Lightness]
    -> global saturation / vibrance
    -> curves / grading / local adjustments
    -> hue-preserving gamut mapping theo output profile
    -> monitor transform hoặc export transform
    -> encode/quantize đúng một lần
```

Working space ban đầu cần được benchmark giữa:

- ACEScg: phù hợp scene-linear, gamut rộng, hệ sinh thái tốt.
- Linear ProPhoto RGB: gần workflow nhiếp ảnh và ART/RawTherapee.

Không chốt bằng cảm tính. Phase 1 phải đo ColorChecker, saturated colors, negative-channel behavior và chi phí GPU trước khi chọn.

Không gian chỉnh Color Mixer ban đầu:

- OKLCh cho SDR/display-referred vì đơn giản, ổn định và đã có một phần Oklab trong IAI.
- JzCzHz là ứng viên cho highlight/HDR; chỉ đưa vào mặc định nếu test chứng minh tốt hơn rõ rệt.

## 5. Bộ dữ liệu và phép đo chuẩn

### 5.1 Test images bắt buộc

- ColorChecker 24 patches dưới D50, D65 và tungsten.
- Hue ramp 0-360 độ ở nhiều mức lightness/chroma.
- Saturation ramp cho red, orange, yellow, green, cyan, blue, purple, magenta.
- Gradient 16-bit để phát hiện banding.
- 8-bit JPEG có chroma subsampling để phát hiện block/seam.
- Ít nhất 10 ảnh da người: sáng, tối, tungsten, mixed light và ngược sáng.
- Hoa/đèn neon/LED để kiểm tra highlight màu.
- Bầu trời và blue fabric để kiểm tra cyan/blue gamut.
- RAW thiếu sáng để kiểm tra shadow noise và tone equalizer.
- RAW dư sáng có một hoặc hai channel gần clip.
- Ảnh sRGB, Adobe RGB, Display P3 và ProPhoto có embedded ICC.

Không commit ảnh có vấn đề bản quyền. Dữ liệu tải ngoài phải ghi nguồn và giấy phép trong manifest.

### 5.2 Các phép đo

- Delta E 2000 hoặc Delta E OK giữa input kỳ vọng và output.
- Hue drift theo độ khi chỉ thay saturation/lightness.
- Delta lightness khi chỉ thay saturation.
- Delta chroma khi chỉ thay lightness.
- Tỷ lệ pixel out-of-gamut trước/sau mapper.
- Số vùng clipping theo từng channel.
- CPU/GPU max absolute error và percentile error.
- Preview-drag so với preview-settled và commit/export.
- Thời gian render 24 MP trên CPU/GPU và VRAM sử dụng.
- Banding: số mức phân biệt còn lại trên gradient và sai số lân cận.

### 5.3 Golden commands cần tạo

Dự kiến thêm integration test/tool:

- `tests/color_pipeline.rs`
- `tests/develop_color_golden.rs`
- `tests/develop_cpu_gpu_parity.rs`
- `tests/color_profile_roundtrip.rs`
- `tools/color_probe.rs` hoặc binary tương đương để xuất CSV/PNG so sánh.

Golden output phải lưu version thuật toán và working space. Không cập nhật golden chỉ để làm test xanh; mọi thay đổi golden cần ghi lý do trong mục Decision log.

## 6. Các phase triển khai

## Phase 0 - Khóa baseline hiện tại

Trạng thái: [x] Hoàn thành

### Công việc

- [x] Thu thập và lập manifest test images (corpus tổng hợp ban đầu; ảnh ngoài còn chờ nguồn hợp lệ).
- [x] Thêm pixel-probe cho `eval_scene_pixel` và pipeline display-referred.
- [x] Render baseline ở neutral và các mức slider -100, -50, +50, +100.
- [x] Chụp baseline CPU, GPU preview và committed tilemap bằng corpus procedural tái lập.
- [x] Đo sai khác preview/commit (`max=1/255`, `P99=1/255` trên headless WGPU baseline).
- [x] Ghi các lỗi chính bằng baseline grid/crop số và regression matrix, không chỉ mô tả cảm giác.
- [x] Ghi hiệu năng CPU hiện tại ở 12 MP, 24 MP và 45 MP (GPU/VRAM còn chờ headless harness).

### File dự kiến

- `src/core/develop_scene.rs`
- `src/core/develop/pipeline.rs`
- `src/gpu/compositor.rs`
- `src/gpu/compositor.wgsl`
- `tests/`
- `docs/color-pipeline/BASELINE_2026-08.md`

### Tiêu chí hoàn thành

- [x] Có thể tái tạo một output bằng command test duy nhất.
- [x] Có báo cáo sai khác CPU/GPU/commit.
- [x] Có ít nhất một regression test bắt nguồn từ từng lỗi chính cần sửa; các test xanh sau sửa lỗi.

## Phase 1 - Color management và working space rộng

Trạng thái: [x] Hoàn thành

### Công việc

- [x] Kiểm kê toàn bộ nơi đang giả định sRGB primaries hoặc Rec.709 luminance.
- [x] Định nghĩa `WorkingColorSpace` và metadata color pipeline rõ ràng.
- [x] Benchmark ACEScg và linear ProPhoto trên bộ test.
- [x] Chốt working space qua ADR/Decision log: linear ProPhoto cho RAW mới.
- [x] Chuyển camera RGB sang working space một lần khi nhập; raster ICC đi qua lcms vào compatibility boundary.
- [x] Giữ working buffer float/half-float và cho phép giá trị âm hoặc lớn hơn 1 khi cần.
- [x] Thực hiện output transform sang sRGB/P3/ICC profile ở boundary.
- [x] Thực hiện monitor transform cho preview qua display LUT; không coi surface sRGB là color management đầy đủ.
- [x] Kiểm tra alpha/premultiplication không bị áp gamma sai.
- [x] Thêm roundtrip tests cho sRGB, P3, Adobe RGB và ProPhoto.

### File dự kiến

- `src/core/cms.rs`
- `src/core/color.rs`
- `src/core/develop_scene.rs`
- `src/formats/raw.rs`
- `src/formats/jpeg.rs`
- `src/formats/png.rs`
- `src/formats/tiff.rs`
- `src/gpu/compositor.rs`
- `src/gpu/compositor.wgsl`
- `src/core/document.rs`

### Tiêu chí hoàn thành

- Neutral edit không đổi màu ngoài sai số transform được định nghĩa.
- Embedded-profile images hiển thị đúng và khác nhau đúng kỳ vọng.
- Không clip wide-gamut colors trước output transform.
- CPU/GPU sử dụng cùng matrices/transfer functions.

## Phase 2 - Perceptual color core dùng chung

Trạng thái: [x] Hoàn thành

### Công việc

- [x] Tạo module chuyển đổi working RGB <-> OKLab/OKLCh.
- [x] Hỗ trợ signed/out-of-gamut RGB an toàn, không NaN khi cube root.
- [x] Thêm JzAzBz/JzCzHz prototype sau feature flag để so sánh highlight.
- [x] Định nghĩa `PerceptualColor { lightness, chroma, hue }` hoặc API tương đương.
- [x] Viết test neutral axis, hue roundtrip, negative RGB và extreme HDR values.
- [x] Đồng bộ constants/công thức CPU-WGSL bằng explicit twins và parity test; chưa sinh tự động vì WGSL hiện là source độc lập.
- [x] Đặt tolerance parity cụ thể cho f32/f16.

### File dự kiến

- `src/core/color.rs`
- `src/core/ucs.rs` sau khi đánh giá khả năng thay thế/giữ lại
- `src/core/perceptual_color.rs` mới
- `src/gpu/compositor.wgsl`
- `build.rs` nếu sinh WGSL/constants tự động

### Tiêu chí hoàn thành

- [x] Roundtrip không NaN/Inf trên toàn bộ test vectors.
- [x] Neutral RGB giữ neutral.
- [x] Hue error và CPU/GPU error nằm dưới tolerance đã ghi trong test.

## Phase 3 - Color Mixer v2

Trạng thái: [~] Code và automated tests hoàn thành; tinh chỉnh cảm giác thanh kéo được hoãn tới sau pipeline

### Thiết kế

Color Mixer v2 dùng cùng một perceptual cylindrical space cho:

- Xác định hue band.
- Thay đổi hue.
- Thay đổi chroma/saturation.
- Thay đổi perceptual lightness.

Hue bands dùng periodic raised-cosine hoặc periodic cubic spline. Mỗi band có:

- Tâm hue rõ ràng.
- Inner range nhận 100% tác động.
- Falloff range liên tục C1/C2.
- Overlap được chuẩn hóa, tránh cộng strength ngoài dự kiến.

### Công việc

- [x] Viết API `ColorMixerV2` độc lập với pipeline cũ.
- [x] Prototype/đo kernel tuần hoàn; raised-cosine đạt isolation/continuity và không overshoot nên được chọn.
- [x] Chọn kernel qua test, không thêm ngoại lệ Red->Orange.
- [x] Thay UCS + HSV confidence + Oklab rotation bằng một mô hình OKLCh thống nhất trong V2.
- [x] Hue slider thay đổi hue, không đổi L/C ngoài sai số gamut.
- [x] Saturation slider thay đổi chroma, cố giữ L/h.
- [x] Luminance slider thay đổi perceptual L, cố giữ C/h.
- [x] Bảo vệ neutral bằng chroma confidence liên tục, không gate cứng.
- [x] Định nghĩa behavior cho near-black và near-white bằng confidence/guard liên tục.
- [x] Thêm engine API targeted eyedropper trả về hue center và mask preview; chưa đưa vào UI chính trước Phase 7.
- [x] Giữ legacy mixer khi mở file cũ bằng version migration `serde(default)`.
- [ ] Chạy A/B IAI v1, IAI v2, ART và Photoshop trên test chart.

### File dự kiến

- `src/core/develop/mixer.rs`
- `src/core/develop/color.rs`
- `src/core/develop/settings.rs`
- `src/core/develop/mod.rs`
- `src/ui/develop.rs`
- `src/gpu/compositor.rs`
- `src/gpu/compositor.wgsl`
- `src/core/presets.rs`
- serialization `.iai`

### Tiêu chí hoàn thành

- Chỉnh một band không tạo đổi màu nhìn thấy ở band đối diện.
- Không có seam tại 0/360 độ.
- Saturation-only giữ perceptual lightness trong tolerance.
- Luminance-only giữ hue trong tolerance, trừ khi gamut mapping buộc phải nén.
- Không còn cần `red_to_orange_falloff` hoặc sign-clamp để vá RBF overshoot.
- Skin, blue sky và neon qua manual review.

## Phase 4 - Hue-preserving gamut mapping

Trạng thái: [x] Hoàn thành — automated tests và nghiệm thu bằng mắt đạt

### Công việc

- [x] Tách scene tone mapping khỏi output gamut mapping.
- [x] Xây gamut boundary/cusp theo output color space.
- [x] Nén chroma theo lightness và hue đến biên gamut thay vì clamp từng channel.
- [x] Đảm bảo mapper gần identity với màu đã nằm trong gamut.
- [x] Chỉ chạy highlight shoulder hiện hữu khi vượt display range; tuning/redesign để Phase 5 xử lý.
- [x] So sánh binary search, cusp LUT và analytic approximation về chất lượng/tốc độ.
- [x] Dùng output-profile-specific mapping cho sRGB/P3; export ICC tiếp tục qua CMS downstream.
- [x] Test monotonicity để tăng saturation không làm chroma giảm bất ngờ trước cusp.
- [x] Test hue continuity quanh red wrap và blue/cyan cusp.

### File dự kiến

- `src/core/cms.rs`
- `src/core/color.rs`
- `src/core/develop_scene.rs`
- `src/core/gamut_map.rs` mới
- `src/gpu/compositor.wgsl`

### Tiêu chí hoàn thành

- Màu trong gamut gần như không đổi.
- Màu ngoài gamut giảm chroma mềm, không đổi hue đột ngột.
- Highlight màu không bị kéo trắng/xám quá mức.
- Không xuất hiện contour/banding trên hue và saturation ramps.

## Phase 5 - Light và tone mapping v2

Trạng thái: [x] Hoàn thành — automated tests và nghiệm thu bằng mắt đạt

### Công việc

- [x] Giữ exposure đúng dạng `2^EV` trong scene-linear.
- [x] Đánh giá lại tone equalizer zone centers, widths và gains trên scene/HDR ramps.
- [x] Tách tone curve tác động lên luminance khỏi chroma compensation.
- [x] Cài ba mode: Perceptual, Film-like và Neutral.
- [x] Perceptual mode ưu tiên da và độ tương phản cảm nhận.
- [x] Film-like mode dùng shoulder/toe mềm, bảo vệ highlight màu.
- [x] Neutral mode giảm tối đa hue/chroma drift.
- [x] Thay highlight chroma compression cố định bằng compression dựa trên mức tone compression và gamut excursion thực.
- [x] Làm Whites/Highlights monotonic và tránh double-compression.
- [x] Làm Blacks/Shadows noise-aware nhưng không dùng mask có bước nhảy.
- [x] Point curve có mode luminance/perceptual và RGB rõ ràng.
- [x] Histogram phản ánh đúng stage đang hiển thị qua shared scene/display chain.

### File dự kiến

- `src/core/develop_scene.rs`
- `src/core/develop/tone.rs`
- `src/core/develop/curves.rs`
- `src/core/develop/settings.rs`
- `src/ui/develop.rs`
- `src/gpu/compositor.rs`
- `src/gpu/compositor.wgsl`

### Tiêu chí hoàn thành

- Exposure tăng/giảm monotonic trên toàn dải.
- Highlights âm phục hồi cảm giác màu thay vì chỉ chuyển xám.
- Không có hue inversion hoặc channel crossing trên ramps.
- Ba mode có khác biệt có chủ đích và được mô tả trong UI.
- CPU/GPU parity đạt tolerance.

## Phase 6 - Full-resolution preview và parity

Trạng thái: [~] Parity màu đã nghiệm thu; còn chặn bởi lag trên ảnh lớn

### Công việc

- [x] Xác định chính xác đường nào còn dùng color proxy downsample.
- [x] Cho Color Mixer v2 chạy full-resolution ở quality/settled mode.
- [x] Interactive mode được dùng proxy/adaptive resolution khi đang kéo.
- [x] Khi debounce hết hạn, render full-quality tự động.
- [x] Không giảm chroma detail mặc định ở quality mode.
- [x] Guided color smoothing thành tùy chọn có strength, mặc định neutral.
- [x] Thêm state badge để biết Interactive / Refining / Full quality.
- [x] So sánh settled preview với commit ở pixel level.
- [x] Giữ LUT dùng chung và mirror gates bắt buộc cho các công thức CPU/WGSL độc lập còn lại.
- [x] Sửa RAW realtime GPU bỏ sót camera RGB curve; người dùng xác nhận Interactive → Refine không còn nhảy màu.
- [x] Không khởi chạy full-resolution refine khi chuột còn đang kéo; giới hạn một CPU bake đang chạy.
- [x] Tối ưu realtime preview trên ảnh RAW lớn bằng coalescing theo frame, cache proxy và proxy local-tone thích ứng theo kích thước/zoom.
- [x] Đo riêng đường preview trên release build với NEF 36,4 MP: nút thắt `finish_region_e` giảm từ mean 25,01 ms xuống 11,07 ms; upload giảm từ 9,1 MB xuống 2,3 MB mỗi tick ở 100%.
- [x] Chỉ cập nhật tối đa một preview cho mỗi frame; dùng viewport/adaptive spatial resolution và cache tài nguyên theo zoom.
- [ ] Cải thiện default RAW look khi embedded JPEG được thay bằng full decode; histogram fit hiện chưa tương đương camera profile/DCP.

### File dự kiến

- `src/core/develop/mod.rs`
- `src/core/develop/spatial.rs`
- `src/core/develop/pipeline.rs`
- `src/app/actions/develop.rs`
- `src/gpu/compositor.rs`
- `src/gpu/compositor.wgsl`

### Tiêu chí hoàn thành

- Settled preview và commit không có khác biệt nhìn thấy; sai số đạt threshold test.
- Không còn block 6x6, halo hue hoặc chroma smearing từ proxy ở quality mode.
- Thả slider phải lên full-quality trong thời gian mục tiêu đã đo.

## Phase 7 - UI/UX và công cụ đánh giá

Trạng thái: [ ] Chưa làm

### Ràng buộc tương thích giao diện

- Giữ nguyên bố cục panel Develop hiện tại.
- Giữ nguyên các nhóm Light, Color, Effects, Curves và Color Mixer.
- Giữ nguyên tab HSL cùng tám dải Reds, Oranges, Yellows, Greens, Aquas, Blues, Purples và Magentas.
- Giữ nguyên vị trí, kiểu slider, tên điều khiển và workflow kéo màu hiện tại, trừ khi có lỗi rõ ràng cần sửa.
- Giữ khoảng giá trị hiển thị hiện tại khi có thể; nếu thuật toán nội bộ đổi scale thì dùng mapping bên trong để UI vẫn quen thuộc.
- Không thêm mode kỹ thuật vào giao diện chính nếu chúng làm panel phức tạp hoặc khác phong cách PTS.
- Không thay đổi UI chỉ để thể hiện kiến trúc color pipeline mới.
- Mọi thay đổi giao diện phải là cải tiến nhỏ, tương thích ngược và không phá muscle memory của người dùng.

### Công việc

- [ ] Giữ nguyên giao diện hiện có và nối các slider hiện tại sang thuật toán Color Mixer/Light mới.
- [ ] Giữ nguyên giá trị/preset cũ ở tầng UI; mapping sang response mới phải thực hiện bên trong engine.
- [ ] Nếu sửa slider gradient, giữ nguyên kích thước và hình thức; chỉ làm màu gradient phản ánh kết quả thật chính xác hơn.
- [ ] Kiểm tra thao tác drag, double-click/reset và nhập số vẫn hoạt động giống hiện tại.
- [ ] Kiểm tra không có bước nhảy khi drag qua hue wrap hoặc khi saturation về 0.
- [ ] Chỉ bổ sung tooltip ngắn nếu cần giải thích khác biệt giữa Saturation, Vibrance và Color Mixer Saturation.
- [ ] Working profile, output profile và tone-map mode chỉ hiển thị trong Advanced/Debug khi thật sự cần thiết.

### Tính năng UI tùy chọn, không thuộc phạm vi bắt buộc

Các mục sau không được làm chậm việc sửa chất lượng màu và không tự động đưa vào giao diện chính:

- Mask preview cho từng color band.
- Targeted-adjustment eyedropper hoặc kéo trực tiếp trên ảnh.
- Điều chỉnh range/falloff của hue band.
- Vectorscope và gamut warning.
- Bộ chọn tone-map mode nâng cao.

Chỉ triển khai các mục này sau khi pipeline lõi ổn định và khi chúng có thể được thêm mà không làm thay đổi giao diện PTS hiện tại. Ưu tiên đặt trong Advanced, menu phụ hoặc công cụ debug.

### File dự kiến

- `src/ui/develop.rs`
- `src/ui/widgets.rs`
- `src/ui/color_picker.rs`
- `src/app/actions/develop.rs`
- `src/gpu/compositor.rs`

### Tiêu chí hoàn thành

- Người dùng cũ sử dụng Color Mixer và Light mà không cần học lại.
- Bố cục, tab, tên và vị trí điều khiển chính không thay đổi.
- Preset và giá trị UI cũ vẫn nạp/hiển thị hợp lý.
- Slider hiện tại điều khiển đúng thuật toán mới và không có bước nhảy khi drag/reset.
- Nếu gradient được cập nhật, hướng thay đổi của gradient phải khớp kết quả thật mà không đổi hình thức tổng thể.

## Phase 8 - Migration, hiệu năng và phát hành

Trạng thái: [ ] Chưa làm

### Công việc

- [ ] Version `DevelopSettings`/Color Mixer algorithm trong document.
- [ ] File cũ mở ra giữ look cũ bằng legacy path hoặc migration có kiểm chứng.
- [ ] Preset cũ không silently đổi look.
- [ ] Benchmark CPU scalar/SIMD và GPU.
- [ ] Kiểm tra VRAM với 45 MP, nhiều layer và local adjustments.
- [ ] Thêm fallback an toàn khi GPU không hỗ trợ texture/buffer cần thiết.
- [ ] Viết release notes và hướng dẫn so sánh legacy/v2.
- [ ] Chỉ xóa legacy path sau ít nhất một chu kỳ phát hành ổn định.

### Tiêu chí hoàn thành

- Không phá file và preset cũ.
- Không regression hiệu năng vượt ngân sách được chốt.
- Golden tests, parity tests và manual review đều đạt.

## 7. Ngân sách chất lượng ban đầu

Các con số dưới đây là mục tiêu khởi điểm; Phase 0 có thể điều chỉnh và phải ghi Decision log:

- CPU/GPU SDR settled output: max channel error <= 2/255, P99 <= 0.5/255.
- Neutral roundtrip 16-bit: không banding nhìn thấy; sai số trung bình <= 1 code value nếu pipeline cho phép.
- Hue-only: Delta L và Delta C nhỏ hơn ngưỡng perceptual trên màu chưa chạm gamut.
- Saturation-only: Delta L không nhìn thấy trên ColorChecker/skin patches.
- Lightness-only: hue drift mục tiêu dưới 1 độ khi chưa chạm gamut.
- Settled preview vs commit: không khác biệt nhìn thấy ở 200% zoom.
- Full-quality GPU preview 24 MP: đặt target sau Phase 0 dựa trên phần cứng chuẩn.
- Không NaN, Inf hoặc negative-to-pow error trên fuzz vectors.

## 8. Test matrix cho mỗi pull request

Mỗi thay đổi trong Color/Light pipeline phải chạy phần phù hợp trong matrix:

| Nhóm | Neutral | Hue | Sat/Vib | Light | Gamut | CPU/GPU | Legacy |
|---|---:|---:|---:|---:|---:|---:|---:|
| sRGB 8-bit | x | x | x | x | x | x | x |
| sRGB 16-bit | x | x | x | x | x | x | x |
| Display P3 | x | x | x | x | x | x | - |
| ProPhoto | x | x | x | x | x | x | - |
| RAW SDR | x | x | x | x | x | x | x |
| RAW highlight/HDR | x | x | x | x | x | x | x |
| JPEG 4:2:0 | x | x | x | x | - | x | x |

Ngoài automated tests, các ảnh sau phải được xem thủ công:

- Da người.
- Blue sky/cyan.
- Red/orange boundary.
- Magenta wrap về red.
- Neon/highlight màu.
- Shadow có noise.
- Gradient 16-bit.

## 9. Rủi ro và cách giảm thiểu

### Đổi working space làm thay đổi toàn bộ look

- Giữ legacy version trong document.
- Chạy cả hai pipeline song song trong giai đoạn chuyển đổi.
- Không migrate preset tự động nếu chưa có look-matching transform.

### OKLCh không xử lý highlight/HDR đủ tốt

- Giữ JzCzHz prototype sau feature flag.
- Chỉ dùng OKLCh ở display-referred stage nếu scene-linear HDR cho kết quả kém.

### Gamut mapper quá chậm

- Prototype analytic/cusp LUT và binary search.
- Sinh LUT theo output profile, cache theo profile hash.
- GPU texture lookup cho preview.

### CPU và WGSL lệch dần

- Sinh constants/LUT từ Rust.
- Thêm parity test bắt buộc.
- Mỗi hàm shader ghi rõ CPU twin và test ID.

### Preview full-resolution quá nặng

- Adaptive interactive resolution khi pointer đang drag.
- Settled render bắt buộc full-quality.
- Benchmark trước khi bỏ hẳn proxy.

### Tuning theo một vài ảnh da

- Dùng chart, nhiều skin tone và ảnh màu bão hòa.
- Mọi threshold phải có test chứng minh mục đích.

## 10. Quy tắc cập nhật tài liệu khi code

Sau mỗi thay đổi có ý nghĩa:

1. Đánh dấu checkbox đã hoàn thành.
2. Ghi commit hoặc PR nếu có.
3. Ghi test đã chạy và kết quả.
4. Ghi ảnh/manual cases đã kiểm tra.
5. Nếu đổi thuật toán hoặc threshold, thêm một mục Decision log.
6. Nếu phát hiện vấn đề mới, thêm vào Open issues, không chèn workaround im lặng.
7. Không đánh dấu phase hoàn thành khi mới chỉ compile hoặc unit test đơn lẻ.

## 11. Decision log

### D-001 - Không tiếp tục tuning RBF hiện tại trước baseline

Ngày: 2026-08-09  
Trạng thái: Chấp nhận tạm thời

Lý do: Mixer hiện tại đã cần nhiều gate, clamp và ngoại lệ. Tuning thêm có nguy cơ cải thiện một nhóm ảnh nhưng làm xấu nhóm khác. Ưu tiên dựng test và mô hình thống nhất trước.

### D-002 - Chưa chốt ACEScg hay linear ProPhoto

Ngày: 2026-08-09  
Trạng thái: Đã quyết định — linear ProPhoto

Lý do: Cả hai đều khả thi. Quyết định phải dựa trên color accuracy, behavior với negative RGB, tương thích ICC/DCP và hiệu năng GPU.

Kết quả Phase 1: trên 500.000 vector signed/HDR, ACEScg có 98.037 negative channels, sai số roundtrip tối đa 0.00010496 và 4.335 ms; linear ProPhoto có 51.987 negative channels, sai số 0.00004029 và 4.310 ms. Chọn linear ProPhoto. Xem `docs/color-pipeline/ADR-001-WORKING-SPACE.md`.

### D-003 - OKLCh là ứng viên mặc định, JzCzHz là prototype

Ngày: 2026-08-09  
Trạng thái: Chấp nhận cho prototype

Lý do: IAI đã có Oklab hue rotation nên chi phí tích hợp thấp. JzCzHz cần chứng minh lợi ích rõ ở highlight/HDR trước khi tăng độ phức tạp.

### D-004 - Giữ nguyên giao diện Develop theo phong cách Photoshop

Ngày: 2026-08-09  
Trạng thái: Chấp nhận, ràng buộc bắt buộc

Lý do: Giao diện hiện tại đã quen thuộc và đúng định hướng sản phẩm. Dự án này tập trung sửa color science, Light pipeline, preview và độ chính xác của thanh kéo. Không redesign panel hoặc thay workflow. Các công cụ mới như mask preview, eyedropper, vectorscope và tone-map mode chỉ là tùy chọn sau khi lõi ổn định, ưu tiên đặt trong Advanced/Debug.

### D-005 - Giới hạn spatial proxy local-tone trong lúc kéo

Ngày: 2026-08-09  
Trạng thái: Chấp nhận cho interactive preview; settled/commit không đổi

Lý do: Release benchmark trên NEF 7380×4928 xác định exposure-plane 1/4 là nút
thắt chính: `finish_region_e` mean 25,01 ms và upload 2.273.040 `f32` (~9,1 MB)
mỗi tick. Interactive preview nay chọn downsample theo số pixel và zoom, giới hạn
600.000 mẫu ở 100% và thấp hơn ở fit view. Đây chỉ là giảm độ phân giải không gian;
WB/exposure, guided-filter, tone-equalizer LUT, shader và CPU settled/commit giữ
nguyên. Trên cùng RAW, proxy 923×616 cho mean 11,07 ms và ~2,3 MB upload/tick.

### D-006 - Khôi phục Color Mixer proxy cho RAW interactive

Ngày: 2026-08-09  
Trạng thái: Chấp nhận; full-resolution refine/commit không đổi

Đối chiếu lịch sử GitHub cho thấy bản mượt trước nâng cấp luôn đặt Color Mixer
trên proxy tương tác. Trong Phase 1–6, nhánh RAW bị đổi sang chạy trực tiếp toàn
bộ scene-linear Color Mixer/OKLCh/gamut chain trong shader cho từng pixel; vì
vậy tối ưu local-tone không thể chữa lag khi Color Mixer đang bật. Interactive
RAW nay lại dùng proxy như kiến trúc cũ, nhưng proxy được tính bằng chính
working-space/color math mới; shader không áp Color Mixer lần hai. Sau khi thả
chuột, CPU refine vẫn render full-resolution bằng pipeline commit.

Do scene color math mới nặng hơn display-domain math cũ, proxy RAW có ngân sách
riêng tối đa khoảng 60.000 texel/frame. Release benchmark trên NEF 36,4 MP:
Fit `scene_color_proxy_region` mean 11,32 ms (296×198); viewport 100% mean
8,64 ms (300×163), đều dưới ngân sách 16,7 ms của một frame 60 Hz.

## 12. Open issues

- [x] Xác định chính xác tất cả đường preview nào còn dùng color proxy 1/6; xem `docs/color-pipeline/PROXY_INVENTORY_2026-08.md`.
- [ ] Kiểm tra monitor ICC hiện có được áp dụng ở đâu và có bị bypass trong WGPU path không.
- [ ] Kiểm tra RAW DCP/ICC transform đang dừng ở camera matrix hay có đầy đủ LUT/look table.
- [ ] Định nghĩa behavior của Color Mixer v2 đối với RGB âm và màu ngoài working gamut.
- [ ] Chọn metric chroma/lightness cho Vibrance.
- [ ] Quyết định local adjustments chạy trước hay sau global Color Mixer v2.
- [ ] Định nghĩa cách histogram/vectorscope phản ánh soft proof và monitor transform.
- [ ] Chốt ngân sách performance sau Phase 0.

## 13. Bước code đầu tiên được phép làm

Không bắt đầu bằng việc sửa saturation scale, hue width hoặc highlight constants.

Thứ tự task đầu tiên:

1. Tạo test vectors và pixel-probe.
2. Ghi baseline neutral và các slider extremes.
3. Viết CPU/GPU/commit parity test.
4. Kiểm kê assumptions sRGB/Rec.709 trong toàn repository.
5. Lập ADR chọn working space.
6. Sau đó mới prototype Color Mixer v2 và gamut mapper.

## 14. Nhật ký tiến độ

| Ngày | Phase | Thay đổi | Test/kết quả | Commit/PR |
|---|---|---|---|---|
| 2026-08-09 | Planning | Tạo kế hoạch từ phân tích ART và IAI | Chưa chạy test, chưa sửa code | - |
| 2026-08-09 | Phase 0 | Thêm corpus vector tổng hợp, `color_probe`, manifest và baseline tái lập | `cargo test --test color_pipeline`: 3 passed; probe tạo 60341-byte CSV | Working tree |
| 2026-08-09 | Phase 0 | Thêm golden test, settled/commit parity, WGSL validation, proxy inventory và benchmark tổng hợp | 6 test passed; release CPU: 12 MP 725.657 ms, 24 MP 1375.473 ms, 45 MP 2644.615 ms | Working tree |
| 2026-08-09 | Phase 0 | Thêm compositor readback và chạy parity thực trên headless WGPU; đóng Phase 0 | GPU/commit max 1/255, P99 1/255; full suite xem báo cáo nghiệm thu | Working tree |
| 2026-08-09 | Phase 1 | WorkingColorSpace, RAW linear ProPhoto, ICC P3/ProPhoto, metadata `.iai`, inventory và ADR | 1173 lib tests + integration/doc tests passed; GPU parity max 1/255; chờ manual profile/monitor review | Working tree |
| 2026-08-09 | Phase 1 | Sửa display LUT encoded/linear boundary và refresh LUT ngay sau Assign/Convert; nghiệm thu ProPhoto bằng mắt | 1173 passed; người dùng xác nhận Convert ProPhoto hiển thị đúng | Working tree |
| 2026-08-09 | Phase 2 | Thêm perceptual core OKLab/OKLCh dùng chung, signed/HDR safety, WGSL twins và JzAzBz prototype feature flag | Unit/parity/WGSL/full suite passed; xem `docs/color-pipeline/PHASE2_PERCEPTUAL_CORE.md` | Working tree |
| 2026-08-09 | Phase 3 | Color Mixer v2 OKLCh, raised-cosine bands, migration Legacy/V2, CPU/WGSL và targeted APIs | Automated suite + headless GPU parity đạt; chờ manual review skin/blue/neon | Working tree |
| 2026-08-09 | Phase 4 | Gamut mapper OKLCh giữ hue/L, boundary sRGB/P3, CPU/WGSL và benchmark binary/LUT/analytic | Automated tests đạt; headless GPU/commit max 2/255, P99 1/255; chờ manual review | Working tree |
| 2026-08-09 | Phase 5 | Tone v2 ba mode, compression theo gamut excursion, shadow noise guard và point curve perceptual/luma | Automated suite + headless Perceptual-curve parity đạt; chờ manual review | Working tree |
| 2026-08-09 | Phase 6 | Debounced full-quality settled bake, preview quality badge và optional Color Smoothing mặc định 0 | Automated suite + direct-quality/parity gates; chờ manual preview→commit review | Working tree |
| 2026-08-09 | Phase 6 | Sửa camera curve bị thiếu ở GPU realtime, single-flight/refine-after-release và panel footer | Người dùng xác nhận hết nhảy màu; headless GPU/commit max 1/255, P99 1/255; còn lag ảnh lớn | Working tree |
| 2026-08-09 | Phase 6 | Coalesce theo display frame, cache proxy/scene resources và local-tone proxy thích ứng kích thước/zoom | Release NEF 36,4 MP: `finish_region_e` mean 25,01→11,07 ms; upload 9,1→2,3 MB/tick; 1200 lib tests + perf regression đạt; chờ nghiệm thu lag bằng mắt | Working tree |
| 2026-08-09 | Phase 6 | Đối chiếu GitHub và khôi phục RAW Color Mixer interactive proxy bằng color math mới | Release NEF 36,4 MP: Fit mean 11,32 ms, 100% mean 8,64 ms; 1200 lib tests, golden/parity CPU và proxy-budget regression đạt | Working tree |
| 2026-08-09 | Phase 6 correction | Loại bỏ lại RAW colour proxy vì gây đổi look; giảm riêng viewport target 2x/3x/4x khi pointer-down, native khi release | Không đổi shader/LUT math; render-scale regression đạt |
| 2026-08-09 | Phase 6 transition | Open Image chờ exact CPU settled frame; embedded JPEG chỉ dùng lấy statistics, không thay thế canvas RAW | Loại bỏ hai đường chuyển ảnh gây pop sau decode/commit |
| 2026-08-09 | Default RAW look | Thêm thumbnail correspondence 24x24 và bounded ridge 3x3 camera-colour fit trước histogram curves | Identity-fit regression + camera-curve regression đạt |

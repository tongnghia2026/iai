# Kế hoạch cải tổ RAW: giảm RAM và nâng chất lượng giải mã/hiển thị

**Ngày lập:** 2026-08-25  
**Trạng thái:** Đã bổ sung track chất lượng ART; sẵn sàng triển khai ở cuộc trò chuyện mới  
**Mức độ:** Nghiêm trọng / ưu tiên cao  
**Corpus chuẩn:** `C:\Users\Admin\Pictures\anh-raw`  
**Mã nguồn tham khảo:** `C:\Users\Admin\Pictures\1111\ART`

---

## 1. Tóm tắt điều hành

Khi mở toàn bộ ảnh trong corpus chuẩn, iAi dùng khoảng 12 GB RAM, trong khi Photoshop được quan sát khoảng 2 GB. Điều tra trên mã nguồn iAi và chạy giải mã thật cho thấy đây chủ yếu **không phải memory leak cổ điển**. Nguyên nhân chính là iAi giữ đồng thời nhiều biểu diễn full-resolution cho tất cả tài liệu RAW đã mở.

Ngoài lỗi RAM, chất lượng RAW hiện tại của iAi được người dùng đánh giá thấp hơn rõ rệt so với ART và Photoshop: màu kém chân thật, chi tiết/độ nét chưa tốt, default look còn nhợt hoặc gắt tùy ảnh, và phản ứng khi kéo exposure, highlights, shadows, saturation hoặc Color Mixer chưa tự nhiên bằng ART. Đây không phải một lỗi có thể giải quyết bằng vài hằng số saturation/sharpen. ART có lợi thế từ cả chuỗi xử lý RAW: sensor preprocessing, demosaic theo loại cảm biến, cơ sở dữ liệu camera, DCP/ICC, working space rộng, tone curve bảo toàn màu, xử lý chi tiết theo scale và color-managed display.

Corpus gồm 20 ảnh RAW và 1 file XMP, tổng khoảng 612,3 megapixel. Chỉ riêng các buffer thường trú đã ước tính khoảng 12,71 GiB:

| Thành phần | Byte/pixel | Ước tính trên corpus |
|---|---:|---:|
| `SceneSource.half`, RGBA f16 | 8 | 4,562 GiB |
| Tile `pixels16`, RGBA16 | 8 | 4,562 GiB |
| Tile `pixels`, RGBA8 mirror | 4 | 2,281 GiB |
| Selection mask | 1 | 0,570 GiB |
| Flat `Canvas.pixels` cho nhóm ảnh không quá 25 MP | 4 | 0,734 GiB |
| **Tổng tối thiểu** | | **12,709 GiB** |

Con số này chưa bao gồm allocator overhead, undo/history, GPU texture và scratch trong lúc giải mã. AHD hiện tại có thể tạo thêm khoảng 50 byte/pixel scratch; riêng ảnh lớn nhất 51,1 MP có thể làm peak tăng thêm khoảng 2,38 GiB.

Giải pháp trọng tâm là thay đổi vòng đời tài liệu theo mô hình đã kiểm chứng trong ART:

1. Danh sách ảnh trong phiên chỉ giữ đường dẫn, metadata, thumbnail và develop parameters.
2. Chỉ ảnh đang hoạt động có RAW working set full-resolution.
3. Preview chính được render theo viewport; zoom 100% xử lý tile/crop nhìn thấy.
4. Batch chạy tuần tự: decode, process, save/spill, release rồi mới sang ảnh tiếp theo.
5. Dữ liệu tab nền được giải phóng hoặc spill xuống scratch disk theo ngân sách RAM toàn cục.

Song song, track chất lượng sẽ học kiến trúc xử lý của ART theo phương pháp clean-room:

1. Dùng ART làm black-box/reference renderer trên cùng RAW và cùng điều kiện, không copy mã.
2. Hoàn thiện sensor preprocessing và chọn demosaic theo chất lượng thực đo.
3. Xây dựng hệ camera profile DCP/ICC có nguồn và giấy phép rõ ràng.
4. Tách technical camera transform khỏi creative/default look; bỏ dần các heuristic khớp JPEG toàn cục.
5. Dùng một scene-linear pipeline làm nguồn sự thật cho preview, kéo thanh điều chỉnh, commit và export.
6. Hiển thị qua monitor ICC đúng một lần, tránh gamma/profile sai hoặc preview khác export.

Không thể đạt mục tiêu chỉ bằng tối ưu allocator hoặc sửa AHD trong khi vẫn giữ 20 canvas full-resolution thường trú.

---

## 2. Phạm vi và mục tiêu

### 2.1. Trong phạm vi

- Mở đồng thời nhiều file RAW từ file picker hoặc thao tác tương đương.
- Chuyển qua lại giữa các ảnh đã mở.
- Hiển thị preview, zoom/pan và chỉnh develop parameters.
- Chuyển ảnh RAW sang tài liệu raster/layer khi thật sự cần.
- Batch bake/export nhiều ảnh.
- Giới hạn resident RAM, peak RAM và scratch memory.
- Duy trì đúng dữ liệu chỉnh sửa, màu sắc và kết quả export.
- Nâng chất lượng sensor preprocessing, demosaic, highlight reconstruction và false-colour suppression.
- Camera characterization bằng DCP/ICC hoặc decoder matrix có provenance rõ ràng.
- Nâng default RAW look, độ chân thật, chi tiết và khả năng kéo sáng/kéo màu.
- Bảo đảm preview GPU, settled CPU, commit và export cùng một pipeline/toán màu.
- Color-managed display bằng ICC màn hình, đồng thời giữ output sRGB/ICC đúng chuẩn.

### 2.2. Ngoài phạm vi của đợt đầu

- Viết lại toàn bộ RAW decoder trong một đợt duy nhất; chỉ thay từng stage có benchmark và regression.
- Sao chép mã nguồn ART.
- Phân phối lại DCP/ICC/profile hoặc asset của ART khi chưa có quyền tương thích.
- Cố làm output iAi giống từng byte của ART/Photoshop; mục tiêu là chất lượng tương đương hoặc tốt hơn, không giả mạo engine khác.
- Đổi toàn bộ canvas engine sang một kiến trúc mới trong một commit.
- Đảm bảo mọi tài liệu raster nhiều layer có thể ở RAM đồng thời không giới hạn.

### 2.3. Mục tiêu định lượng

Với corpus chuẩn 20 RAW:

- Milestone kiến trúc: steady resident RAM không quá 3 GiB sau khi mở toàn bộ và đứng yên.
- Milestone hoàn chỉnh: steady không quá 2,5 GiB; peak không quá 3,5 GiB trong luồng thông thường.
- Không quá một RAW working set full-resolution thường trú; có thể thêm một prefetch nếu budget cho phép.
- Batch 20 ảnh phải có RAM dạng plateau, không tăng tuyến tính theo số ảnh.
- Lặp ba vòng mở, dùng và đóng corpus không làm resident RAM tăng liên tục.
- Với các milestone chỉ refactor ownership/RAM, kết quả export phải khớp phiên bản hiện tại trong tolerance được định nghĩa trước.
- Với milestone chủ động nâng chất lượng, phải có golden version mới và báo cáo A/B; không được âm thầm thay look của tài liệu cũ.
- Neutral ColorChecker mục tiêu trung hạn: median Delta E 2000 không quá 3 và P95 không quá 6 khi có profile camera hợp lệ.
- Preview GPU so với settled CPU/commit: tối đa 1/255 trên ít nhất 99% sample SDR; sai khác lớn nhất phải được giải thích ở stage có approximation.
- Các slider chính phải đơn điệu, không đổi hue bất thường, không làm highlight màu chuyển xám sớm và không tạo halo/zipper mới.

Photoshop khoảng 2 GB chỉ nên dùng làm điểm tham khảo UX; không dùng làm oracle kỹ thuật vì Task Manager có thể không phản ánh scratch disk, GPU memory, compression và lazy decode.

---

## 3. Dữ liệu điều tra đã xác nhận

### 3.1. Corpus

- 21 file: 20 RAW và 1 XMP không hỗ trợ.
- Tổng dung lượng file khoảng 0,919 GB.
- Định dạng: 8 CR2, 7 NEF, 3 ARW, 1 CR3, 1 RAF và 1 XMP.
- Tổng output khoảng 612,3 MP.
- 20 RAW đều decode/render thành công trong release test.
- Thời gian test đã quan sát: khoảng 382,4 giây.

Lệnh tái hiện:

```powershell
$env:IAI_RAW_CORPUS='C:\Users\Admin\Pictures\anh-raw'
cargo test --release --test raw_corpus_probe -- --ignored --nocapture
```

Test hiện có: `tests/raw_corpus_probe.rs`.

### 3.2. Điểm giữ bộ nhớ trong iAi

- `src/app/file_ops/open.rs`: batch loader decode tuần tự nhưng sau đó gắn tất cả canvas hoàn chỉnh vào các tab nền. Tuần tự chỉ giảm decode concurrency, không giảm steady-state RAM.
- `src/formats/raw.rs`: RAW được dựng thành scene full-resolution; AHD scratch hiện tỷ lệ theo toàn ảnh.
- `src/core/canvas/mod.rs`: canvas có thể giữ thêm flat RGBA8 đối với ảnh không quá 25 MP.
- `src/core/tile.rs`: tile có `pixels` RGBA8 và tùy chọn `pixels16`; RAW 16-bit có thể giữ cả hai.
- `src/core/selection.rs`: selection mask được cấp theo toàn kích thước canvas.
- `src/core/command.rs` và `src/core/hw.rs`: history budget hiện thiên về từng canvas; nhiều canvas có thể làm tổng budget vượt mức hợp lý.

### 3.3. Kết luận về leak

Hiện chưa có bằng chứng về một object bị thất lạc hoặc reference cycle là nguyên nhân chính. Mức 12 GB khớp với tổng buffer hợp lệ đang được giữ theo thiết kế. Vẫn phải thêm telemetry và soak test để phát hiện leak thứ cấp, nhưng không nên bắt đầu bằng việc săn leak chung chung.

---

## 4. Bài học đã kiểm tra từ ART

ART mặc định có `tabbedUI = false`. Khi nhiều ảnh được chọn, `FileBrowser::openRequested` chỉ chuyển ảnh cuối cùng vào editor full-resolution. Các ảnh còn lại ở browser dưới dạng thumbnail/metadata.

Các vị trí đã kiểm tra:

- `C:\Users\Admin\Pictures\1111\ART\rtgui\options.cc`, khoảng dòng 546: Single Editor Mode là mặc định.
- `C:\Users\Admin\Pictures\1111\ART\rtgui\filebrowser.cc`, khoảng dòng 2457-2469: chỉ mở ảnh cuối khi không dùng tabbed editor.
- `C:\Users\Admin\Pictures\1111\ART\rtengine\rtthumbnail.cc`, khoảng dòng 272-352: lấy embedded thumbnail, resize rồi giải phóng RAW object.
- `C:\Users\Admin\Pictures\1111\ART\rtgui\editorpanel.cc`, khoảng dòng 1157: preview scale mặc định là 10.
- `C:\Users\Admin\Pictures\1111\ART\rtengine\improccoordinator.cc`, khoảng dòng 740-776: các preview buffer được cấp theo kích thước preview.
- `C:\Users\Admin\Pictures\1111\ART\rtengine\ahd_demosaic_RT.cc`, khoảng dòng 36 và 84: AHD tile 144, scratch khoảng 1,03 MiB mỗi core.
- `C:\Users\Admin\Pictures\1111\ART\rtengine\amaze_demosaic_RT.cc`, khoảng dòng 66: xử lý tile để giảm memory và hỗ trợ đa luồng.
- `C:\Users\Admin\Pictures\1111\ART\rtengine\simpleprocess.cc`, khoảng dòng 623-649: batch xử lý từng job; `flushRawData()` và `flushRGB()` được gọi trong pipeline.

ART vẫn có thể tăng RAM mạnh nếu người dùng bật Tabbed Editor và mở nhiều editor đầy đủ. Vì vậy không nên mô tả ART như một hệ thống LRU hoàn hảo. Phần cần học là browser/editor separation, single active full working set, preview giảm tỷ lệ, crop-local work và batch tuần tự.

ART dùng GPLv3; iAi dùng MIT. Chỉ áp dụng ý tưởng và tự viết triển khai. Không copy/paste mã ART vào iAi.

### 4.1. Cách ART giải mã và dựng ảnh có chất lượng cao

Các điểm sau đã được xác nhận trực tiếp trong tree ART tại commit `e166d439c949fddf70dbb92d59cbec3d8dc80c2b`:

1. **Sensor preprocessing trước demosaic.** `RawImageSource::preprocess` xử lý black/white level, optical-black correction khi có, dark frame/flat field, hot/dead pixel, PDAF defects, green equilibration, lens gain map/vignette và chromatic aberration. Demosaic không phải stage đầu tiên đọc mosaic thô rồi nội suy ngay.
2. **Demosaic chuyên biệt theo cảm biến và tình huống.** Bayer mặc định của bản ART đã kiểm tra là RCD; ART còn có AMaZE, AHD, LMMSE, dual-demosaic và các lựa chọn khác. X-Trans mặc định dùng 3-pass. AHD/AMaZE dùng tile để hạ scratch memory. Kết luận cho iAi không phải “copy AMaZE”, mà là benchmark các thuật toán được triển khai độc lập và chọn theo sensor/detail/noise.
3. **False-colour correction ở full resolution.** Sau demosaic, ART có bước sửa false colour riêng thay vì trông chờ sharpening hoặc chroma blur che khuyết điểm.
4. **Camera characterization có dữ liệu lớn.** Tree đã kiểm tra chứa khoảng 160 DCP, 35 input ICC và `camconst.json` với hàng trăm camera records. ART ưu tiên camera ICC/DCP và có decoder matrix fallback. Đây là lợi thế rất lớn so với iAi hiện không có thư mục `camera_profiles` được phân phối trong repository.
5. **DCP technical transform được tách khỏi look.** Pipeline có lựa chọn HueSatMap, LookTable, profile tone curve và baseline exposure offset. Default color management của build đã kiểm tra dùng camera ICC, working profile Rec.2020 và output profile sRGB; các profile đi kèm có thể chọn Auto-Matched Curve hoặc film curve.
6. **Scene/working space là float và rộng.** RAW được chuyển vào working profile trước phần lớn thao tác tone/color. Không clip sớm xuống sRGB 8-bit.
7. **Tone và color là một pipeline có thứ tự.** White balance/exposure, highlight recovery, input transform, tone curve, saturation/local contrast, denoise/sharpen, transform và output/monitor conversion không được trộn thành các heuristic áp trực tiếp lên scene master.
8. **Display được quản lý màu.** ART tách working profile, output profile và monitor transform. Preview màn hình đi qua monitor profile, còn export đi qua output profile. Vì vậy “ảnh nhìn đẹp/đúng” không chỉ phụ thuộc pixel render mà còn phụ thuộc đường hiển thị.
9. **Sharpening có nhiều chế độ và hỗ trợ deconvolution.** Profile `Sharpening.arp` bật Richardson–Lucy deconvolution với radius tự động; ART không dùng một unsharp constant duy nhất cho mọi camera/ISO/scale.
10. **Auto-Matched Curve là lựa chọn có kiểm soát.** ART có profile khớp histogram/tone theo camera preview, nhưng đây là một profile/recipe rõ ràng, không thay thế camera characterization và không mặc định bake một loạt taste constants vào RAW master.

Các file trọng tâm để tiếp tục đối chiếu bằng đọc/black-box test:

- `ART\rtengine\rawimagesource.cc`: preprocess, demosaic dispatch, color-space conversion và highlight recovery.
- `ART\rtengine\procparams.cc`: default RCD/X-Trans, color-management defaults và tham số sharpen/tone.
- `ART\rtengine\dcp.cc` và `dcp.h`: DCP selection/application.
- `ART\rtengine\improccoordinator.cc`: thứ tự preview pipeline và monitor/output conversion.
- `ART\rtengine\improcfun.cc`, `iptonecurve.cc`, `iprgb2out.cc`: tone/color/output stages.
- `ART\rtdata\profiles\*.arp`: recipe được công bố dưới dạng dữ liệu cấu hình.

### 4.2. Khoảng cách hiện tại của iAi

iAi không bắt đầu từ số 0. Mã hiện có đã hỗ trợ scene-linear f16, Linear ProPhoto, ACEScg, CAT16 white balance, output gamut mapping, DCP parser/resolver, dual-illuminant interpolation, ForwardMatrix, technical HueSatMap, camera ICC validation, monitor ICC và CPU/GPU parity tests. Các nền móng này phải được giữ và hoàn thiện, không viết lại tùy tiện.

Khoảng cách đã thấy trong code hiện tại:

- Không có bộ DCP/ICC camera được phân phối cùng repository. Phần lớn RAW không phải DNG có khả năng rơi xuống decoder matrix.
- DCP parser đọc creative `ProfileLookTable` và `ProfileToneCurve`, nhưng `dcp_transform.rs` chủ ý chưa áp dụng chúng. Vì vậy “profile-backed look” vẫn chưa đầy đủ.
- Camera ICC đã có adapter nhưng generic RAW path vẫn đang gated vì domain `[0,1]` không giữ được negative/HDR scene values.
- Khi không có DCP, `raw.rs` mặc định khớp brightness, ma trận màu và RGB histogram curve theo embedded JPEG; sau đó còn dùng các hằng taste như chroma enrich, color NR, capture sharpen và shadow vibrance. Các heuristic này giúp một vài ảnh nhưng không phải camera model vật lý, dễ làm ảnh khác nhợt, gắt, sai hue hoặc mất texture.
- Sensor preprocessing của iAi mới tập trung vào levels, WB, defect/highlight xử lý và demosaic; chưa có độ rộng/bề dày theo camera như ART về optical black, PDAF, green equilibration, CA, gain maps và noise model.
- iAi chọn AHD toàn ảnh tới khoảng 64 MP rồi fallback Malvar. ART mặc định RCD và có nhiều lựa chọn sensor-aware. Cần đo crop 100%, không kết luận AHD luôn tốt hơn chỉ từ tên thuật toán.
- Capture sharpen và color NR đang bake vào scene master bằng hằng môi trường/toàn cục. Điều này làm khó profile theo ISO/camera, khó tắt hoàn toàn và khó duy trì một technical neutral master.
- Camera JPEG fit đang vừa là fallback brightness vừa là creative look approximation. Hai trách nhiệm này cần tách rời.

### 4.3. Nguyên tắc áp dụng ART mà không vi phạm bản quyền

- ART chỉ được dùng như tài liệu nghiên cứu và black-box oracle. Không copy source, LUT, profile, constant table, comment hoặc test vector có nguồn GPL vào iAi.
- Mỗi implementation mới phải dựa trên paper/spec công khai, tiêu chuẩn DNG/ICC, thuật toán tự thiết kế hoặc dependency có license tương thích; commit ghi rõ nguồn.
- Không lấy 160 DCP, 35 ICC, `camconst.json` hoặc `.arp` từ ART để đóng gói lại. Trước mọi asset camera phải có manifest nguồn, tác giả, license, checksum và quyền phân phối.
- Có thể dùng bản ART cài cục bộ để tạo output tham chiếu nội bộ cho A/B test. Không ship output/profile có điều kiện phân phối không rõ như một phần của sản phẩm.
- Ưu tiên asset do iAi tự tạo từ ColorChecker/camera calibration, DCP nhúng trong chính DNG của người dùng, profile người dùng tự cung cấp, hoặc dataset có giấy phép được review riêng.
- Giữ một `docs/legal/RAW_REFERENCE_PROVENANCE.md` và CI gate: asset camera thiếu license/provenance không được đóng gói.

---

## 5. Kiến trúc đích

### 5.1. Phân biệt ba khái niệm

#### `RawSessionEntry`

Đại diện nhẹ cho một ảnh đã được đưa vào phiên:

- `id`, đường dẫn và file identity.
- EXIF/orientation/dimensions.
- Embedded thumbnail hoặc cache key.
- Develop parameters và dirty state.
- Trạng thái residency.
- Scratch artifact nếu từng materialize.
- Error/cancel state.

Không chứa canvas full-resolution hoặc scene RGBA16 thường trú.

#### `RawWorkingSet`

Chỉ tồn tại cho ảnh đang hoạt động hoặc prefetch được budget chấp nhận:

- RAW decoder/source state cần thiết.
- Dữ liệu sensor/preprocessed cần thiết.
- Preview pipeline và cache tile nhìn thấy.
- Cancellation generation.
- Estimated/actual resident bytes.

#### `RasterDocument`

Tài liệu canvas/layer hoàn chỉnh. Chỉ tạo khi:

- Người dùng commit/open RAW thành ảnh chỉnh sửa raster.
- Một công cụ bắt buộc cần layer/canvas mutable.
- Batch cần một output trung gian, nhưng output đó phải được giải phóng/spill ngay sau save.

Đây là điểm khác biệt quan trọng giữa RAW develop và editor raster tổng quát. Không được ép mọi RAW vừa mở thành `RasterDocument` ngay từ đầu.

### 5.2. Trạng thái residency đề xuất

```text
MetadataOnly
  -> ThumbnailReady
  -> PreviewResident
  -> FullRawResident
  -> RasterResident
  -> Spilled
  -> Failed
```

Chuyển trạng thái phải có cancellation token/generation để kết quả decode cũ không được attach vào ảnh đã chuyển hoặc đóng.

### 5.3. Quy tắc thường trú

- Ảnh nền thông thường: `ThumbnailReady` hoặc `Spilled`.
- Ảnh active: tối đa `FullRawResident` hoặc `RasterResident`.
- Prefetch: tối đa một `PreviewResident`/`FullRawResident`, chỉ khi đủ budget.
- Batch: tối đa một job ở pha full-resolution.
- Không attach một `Canvas` mới vào tab nếu budget manager chưa reserve thành công.

---

## 6. Kế hoạch triển khai theo milestone

## Memory Milestone M0 — Telemetry và baseline

### Công việc

1. Thêm bộ đếm logical bytes cho:
   - RAW source/sensor buffer.
   - Scene f16.
   - Tile RGBA16.
   - Tile RGBA8.
   - Flat canvas.
   - Selection mask.
   - History.
   - Decode/demosaic scratch.
   - Preview cache.
   - Scratch disk và GPU nếu đo được.
2. Ghi current, peak và owner document trong debug diagnostics.
3. Bổ sung phép đo process working set trên Windows cho benchmark; không dùng working set thay cho logical accounting.
4. Mở rộng `raw_corpus_probe` hoặc thêm integration harness cho open-all lifecycle.
5. Lưu baseline release build trước thay đổi.

### Hoàn thành khi

- Báo cáo logical memory giải thích được phần lớn working set đã quan sát.
- Có peak theo từng giai đoạn decode/open/attach/switch/close.
- Test có thể chạy lại bằng một lệnh và xuất summary máy đọc được.

---

## Memory Milestone M1 — Mở nhiều RAW dưới dạng session nhẹ

### Công việc

1. Thay luồng `open.rs` để file picker tạo các `RawSessionEntry` trước.
2. Trích embedded thumbnail/metadata cho từng file, có giới hạn concurrency nhỏ.
3. Chỉ activate/decode full ảnh được chọn làm active.
4. Khi người dùng chuyển ảnh:
   - Persist develop parameters.
   - Cancel preview cũ.
   - Evict working set cũ nếu không được budget giữ lại.
   - Activate ảnh mới.
5. Giữ thứ tự tab/session và trạng thái lỗi riêng từng file.
6. XMP phải bị bỏ qua/báo unsupported mà không làm fail cả batch.
7. UI cần phân biệt loading thumbnail, loading preview, resident và spilled.

### Tương thích

- Luồng mở ảnh raster thông thường có thể tiếp tục tạo `RasterDocument` như hiện tại.
- Chỉ chuyển luồng RAW sang session-first.
- Nếu có setting cũ kỳ vọng mở tất cả thành canvas, giữ chế độ opt-in với cảnh báo ước tính RAM; không dùng làm mặc định.

### Hoàn thành khi

- Mở 20 RAW chỉ tạo một full working set.
- Steady RAM không quá 3 GiB trên corpus chuẩn.
- Chuyển qua lại tất cả ảnh không mất develop parameters.

---

## Memory Milestone M2 — Loại bỏ nhân bản canvas không cần thiết

### Công việc

1. Refactor tile storage thành biểu diễn loại trừ, ví dụ:

```rust
enum TilePixels {
    Rgba8(Vec<u8>),
    Rgba16(Vec<u16>),
}
```

   Chỉ tạo conversion cache tạm khi renderer/backend yêu cầu; cache phải evictable.
2. Không tạo flat `Canvas.pixels` khi tile storage đã là source of truth.
3. Selection mask chuyển sang lazy allocation; `None` mang nghĩa không có selection hoặc select-all theo contract rõ ràng.
4. Sau bake/import, xác định một source of truth; giải phóng scene hoặc buffer trung gian không còn cần.
5. History budget chuyển từ từng canvas sang budget tổng toàn ứng dụng.
6. Thêm `estimated_resident_bytes()` cho canvas/tile/selection/history để budget manager ra quyết định.

### Rủi ro

- Renderer có thể đang giả định luôn có RGBA8 mirror.
- Save/export hoặc filter có thể đọc trực tiếp flat canvas.
- Semantics `None` của selection phải được audit kỹ để tránh đảo nghĩa select-all/select-none.

### Hoàn thành khi

- Một RAW 16-bit không giữ đồng thời bản RGBA16 và RGBA8 full-resolution nếu không có consumer hoạt động.
- Các test raster, selection, undo và export hiện có đều qua.

---

## Memory Milestone M3 — Batch tuần tự và spill

### Công việc

1. Thiết kế queue chỉ cho một job ở pha full-resolution.
2. Mỗi job chạy:

```text
load/decode -> process -> encode/save hoặc spill -> drop output -> next
```

3. Không truyền ownership của N canvas hoàn chỉnh vào queue.
4. Khi người dùng muốn giữ output như tab raster, serialize tab nền xuống scratch sau khi tạo thumbnail/composite cần thiết.
5. Scratch artifact phải có:
   - Version/schema.
   - Source fingerprint.
   - Atomic write/rename.
   - Cleanup khi đóng phiên và cleanup stale khi khởi động.
6. Cancel giữa batch phải giải phóng working set hiện tại và giữ output đã hoàn thành hợp lệ.

### Hoàn thành khi

- Batch toàn corpus có RAM plateau.
- Không còn tăng resident gần tuyến tính theo số job đã hoàn thành.
- Crash/restart không làm load nhầm scratch hỏng.

---

## Memory Milestone M4 — Tiled AHD và scratch có giới hạn

### Công việc

1. Chuyển scratch AHD toàn ảnh thành tile có halo đủ cho neighborhood của thuật toán.
2. Viết triển khai riêng dựa trên thuật toán hiện tại; chỉ tham khảo nguyên tắc tile từ ART.
3. Dùng buffer pool, tái sử dụng scratch giữa tile và giữa ảnh.
4. Số worker được tính từ memory budget, không chỉ từ CPU count.
5. Có fallback giảm concurrency nếu reserve scratch thất bại.
6. Kiểm tra biên tile, orientation, Bayer pattern và tất cả định dạng corpus.

### Kiểm thử chất lượng

- So sánh output tiled với output whole-frame hiện tại.
- Định nghĩa tolerance theo channel và báo max/mean error.
- Tạo seam detector ở ranh giới tile.
- Bao phủ ảnh có kích thước không chia hết cho tile và ảnh rất nhỏ.

### Hoàn thành khi

- Scratch peak tỷ lệ với `tile_size × worker_count`, không tỷ lệ với megapixel toàn ảnh.
- Không có seam nhìn thấy hoặc sai khác ngoài tolerance.

---

## Memory Milestone M5 — Preview theo viewport và crop-local 1:1

### Công việc

1. Hiển thị embedded preview ngay khi mở.
2. Chọn preview scale từ viewport và zoom; mục tiêu ban đầu khoảng 1/8 đến 1/12 mỗi chiều cho overview.
3. Ở zoom 100%, chỉ render tile nhìn thấy cộng halo filter.
4. Cache tile theo khóa gồm source revision, develop params hash, scale và coordinates.
5. Invalidate theo dependency; không xóa toàn cache nếu chỉ một tham số cục bộ thay đổi.
6. Ưu tiên tile giữa viewport; prefetch vòng ngoài ở priority thấp.
7. Mọi task phải kiểm tra generation trước khi publish kết quả.

### Hoàn thành khi

- Overview không yêu cầu tạo full rendered canvas.
- Zoom/pan 100% có latency chấp nhận được và RAM ổn định.
- Chuyển ảnh nhanh không attach nhầm preview của ảnh trước.

---

## Memory Milestone M6 — Global resident budget manager

### Chính sách mặc định

```text
budget = min(25% RAM vật lý, 3 GiB)
```

Cho phép người dùng cấu hình, nhưng phải có minimum an toàn cho một active document. Nếu một thao tác đơn lẻ vượt budget, hệ thống báo rõ và chạy với concurrency thấp/spill thay vì silently overcommit.

### API tối thiểu

- `reserve(owner, class, bytes) -> Reservation | Denied`
- `update_actual(reservation, bytes)`
- `release(reservation)`
- `request_eviction(bytes_needed)`
- Snapshot diagnostics theo owner/class.

### Thứ tự eviction

1. Preview tile ngoài viewport.
2. Thumbnail/preview cache có thể dựng lại.
3. Prefetch working set.
4. Full RAW working set của tab nền.
5. History cũ vượt mức ưu tiên.
6. Raster document tab nền được spill an toàn.

Không evict dữ liệu dirty nếu chưa persist thành công.

### Hoàn thành khi

- Mở file, chuyển tab, zoom và batch đều đi qua cùng một budget manager.
- Không có subsystem tự đặt một budget riêng bằng phần lớn RAM máy.
- Diagnostics giải thích được đối tượng nào ngăn eviction.

---

## 6B. Track chất lượng RAW theo kiến trúc ART

Track này chạy song song nhưng không được làm trễ bản sửa RAM khẩn cấp. Mọi thay đổi chất lượng phải version recipe để tài liệu `.iai` cũ vẫn render như trước.

## Quality Milestone Q0 — Reference harness và baseline nhìn thấy được

### Công việc

1. Khóa ART reference ở commit đã kiểm tra và ghi build/profile/setting dùng cho từng output.
2. Chọn một bộ ảnh chất lượng gồm:
   - Toàn bộ corpus hiện tại.
   - ColorChecker dưới daylight và tungsten nếu có.
   - Da người nhiều tông màu.
   - Tóc/vải/lá cây và đường chéo mảnh để bắt zipper/moire.
   - Ảnh ISO thấp/cao, thiếu sáng, dư sáng và channel clip.
   - Neon/LED/hoa bão hòa để kiểm tra highlight hue/gamut.
   - Bayer, X-Trans và linear DNG.
3. Tạo ba lớp so sánh, không trộn lẫn:
   - **Technical neutral:** cùng WB, exposure, camera profile và output sRGB; tắt creative look khi công cụ cho phép.
   - **Default pleasing look:** thiết lập mặc định thật của iAi, ART và Photoshop/ACR.
   - **Slider behavior:** sweep từng control `-100/-50/0/+50/+100`, giữ các control khác neutral.
4. Render full-size TIFF/PNG 16-bit bằng ART CLI hoặc batch, và iAi headless. Lưu command/profile hash bên cạnh output.
5. Sinh crop chuẩn 100% tại skin, foliage, fine lines, shadow noise, highlight và saturated colors.
6. Đo tự động:
   - Delta E 2000/Oklab trên chart và patch ổn định.
   - Luma/chroma/hue theo shadow–midtone–highlight.
   - Edge acutance/overshoot, false-colour energy và noise spectrum.
   - Clipping count, highlight hue drift và out-of-gamut count.
   - CPU preview, GPU preview, settled commit và export parity.
7. Có contact sheet A/B/Blink cho đánh giá bằng mắt trên màn hình đã color-manage.

### Test hiện có phải tận dụng

- `tests/raw_look_probe.rs`
- `tests/dcp_reference_probe.rs`
- `tests/raw_corpus_probe.rs`
- `tests/develop_cpu_gpu_parity.rs`
- `tests/develop_color_golden.rs`
- `tests/color_reference_probe.rs`
- `tests/color_profile_roundtrip.rs`

### Hoàn thành khi

- Mỗi nhận xét “nhợt”, “sai màu”, “mềm”, “gắt” đều ánh xạ được tới ảnh/crop/metric và stage nghi vấn.
- Có baseline iAi–ART cho neutral, default look và tối thiểu năm slider chính.
- Không tune constant chỉ dựa trên một ảnh hoặc embedded JPEG duy nhất.

---

## Quality Milestone Q1 — Sensor preprocessing và normalized RAW master

### Pipeline mục tiêu trước demosaic

```text
decode mosaic + metadata
-> active area / orientation metadata
-> per-channel black + optical-black correction
-> white/saturation level + gain-map normalization
-> bad/PDAF/hot/dead pixel correction
-> optional dark/flat-field correction
-> green-channel equilibration / row-column noise correction
-> lens shading + raw chromatic-aberration correction
-> white balance normalization
-> highlight-state preparation
-> demosaic
```

### Công việc

1. Audit metadata thực tế rawloader/rawler trả về cho từng file corpus; không suy đoán trường không có.
2. Tạo `RawSensorMetadata` có provenance cho black/white level, active area, CFA, WB, gain map, optical black, PDAF mask, ISO và lens data.
3. Mỗi correction là stage riêng, có enable flag, estimated scratch và test neutral no-op.
4. Không dùng observed maximum như camera white level nếu metadata đáng tin; fallback phải có diagnostics vì ảnh thiếu sáng không được tự nâng thành trắng.
5. Xây defect map/pixel correction trước demosaic để không lan defect thành chấm màu.
6. Thêm green equilibration chỉ khi sensor/pattern/diagnostic yêu cầu; tránh blur chi tiết ở ảnh sạch.
7. Chuyển các hằng color NR/capture sharpen taste khỏi “technical normalized master”. Chúng trở thành recipe/profile stage có version.

### Hoàn thành khi

- Technical neutral master không chứa creative warmth/saturation/tone.
- Không còn hot pixel/PDAF/green split nổi bật trên crop chuẩn.
- Mọi fallback level/profile được ghi trong diagnostics và `.iai` provenance.

---

## Quality Milestone Q2 — Demosaic chất lượng cao, tiled và sensor-aware

### Quyết định

Không port mã RCD/AMaZE/AHD từ ART. Đánh giá các phương án triển khai độc lập từ paper/spec hoặc dependency có license tương thích. ART là reference output và nguồn gợi ý về stage/order, không phải nguồn code.

### Công việc

1. Giữ AHD hiện tại làm baseline nhưng chuyển tiled theo Memory Milestone M4.
2. Thêm ít nhất một demosaic chất lượng cao độc lập cho Bayer để so với AHD/Malvar; RCD là ứng viên vì ART dùng mặc định, nhưng chỉ chọn sau benchmark.
3. Nghiên cứu dual-demosaic: thuật toán sắc ở vùng có cấu trúc, thuật toán sạch ở vùng phẳng, blend bằng edge/contrast mask có transition mượt.
4. X-Trans phải có pipeline riêng; không ép CFA không-Bayer qua giả định Bayer.
5. False-colour suppression chạy sau demosaic, cường độ dựa trên artifact/noise thay vì cố định hai vòng cho mọi ảnh.
6. Tile halo và border policy phải giống giữa preview/full export; crop 100% không được đổi texture khi pan.
7. Demosaic selection trở thành recipe versioned, có `Auto/Quality/Fast` nhưng `Auto` được quyết định bằng sensor + zoom + export intent.

### Hoàn thành khi

- Crop đường chéo/tóc/vải không có zipper hoặc màu giả đáng kể hơn ART reference.
- Chi tiết không bị tạo bằng oversharpen; acutance tăng nhưng overshoot/halo nằm trong ngưỡng.
- Peak scratch đạt mục tiêu RAM và output không có tile seam.

---

## Quality Milestone Q3 — Camera profile system có asset hợp pháp

### Hiện trạng cần sửa

iAi đã có resolver theo thứ tự explicit DCP, embedded DNG DCP, exact manifest DCP, trusted camera ICC rồi decoder matrix. Tuy nhiên repository hiện không phân phối `camera_profiles`, creative LookTable/ToneCurve chưa được áp dụng và generic RAW camera ICC còn gated.

### Công việc

1. Giữ thứ tự resolution deterministic:

```text
user-selected exact profile
-> DCP embedded trong DNG
-> bundled exact-match profile đã duyệt license
-> trusted user manifest profile
-> decoder camera matrix fallback
```

2. Tạo Profile Manager UI/diagnostics hiển thị profile đang dùng, illuminant, source, hash và fallback reason.
3. Hoàn thiện technical DCP path:
   - ColorMatrix/ForwardMatrix.
   - Dual-illuminant interpolation.
   - Camera neutral/WB và chromatic adaptation.
   - Technical HueSatMap trong domain an toàn.
   - CameraCalibration/AnalogBalance chỉ sau khi parser/spec/test đầy đủ; không silently ignore nếu profile phụ thuộc chúng.
4. Tách creative DCP data:
   - `ProfileLookTable` và `ProfileToneCurve` là optional look, không bake vào technical master.
   - Người dùng có thể chọn `Neutral`, `Camera/Profile look`, hoặc iAi default look.
5. Thiết kế camera ICC HDR-safe hoặc tiếp tục gate rõ ràng; không clamp negative/headroom vào `[0,1]` chỉ để profile chạy được.
6. Xây `camera_profiles/manifest.json` chỉ sau legal review. Mỗi record có make/model exact aliases, profile hash, nguồn, license, calibration illuminants và test camera.
7. Tạo công cụ nội bộ sinh DCP iAi từ ColorChecker shots; asset tự tạo phải lưu chart reference, illuminants và validation report.
8. Không lấy profile/alias database trực tiếp từ ART. Có thể so coverage count để đặt mục tiêu, nhưng dữ liệu phải có nguồn độc lập.

### Mốc coverage

- Đợt 1: tất cả camera xuất hiện trong corpus chuẩn.
- Đợt 2: 20 model phổ biến của người dùng mục tiêu.
- Đợt 3: mở rộng theo telemetry opt-in/failure reports, không fuzzy-apply profile sai camera.

### Hoàn thành khi

- Corpus dùng profile exact hoặc có fallback reason rõ ràng.
- ColorChecker đạt mục tiêu Delta E khi có profile hợp lệ.
- Đổi illuminant/WB không tạo hue discontinuity.
- Không có asset thiếu provenance/license trong package.

---

## Quality Milestone Q4 — Default RAW look trung tính nhưng đẹp

### Vấn đề hiện tại

`src/formats/raw.rs` đang dùng embedded JPEG để fit baseline RGB gain, ma trận 3×3, histogram RGB curves, sau đó có thể áp chroma enrichment, brightness, shadow vibrance, color NR và capture sharpen. Embedded JPEG mang picture style, contrast, saturation, noise reduction và sharpening của camera; fit toàn cục không thể tái tạo chính xác quan hệ màu không gian và dễ overfit.

### Kiến trúc mới

```text
technical scene master
-> camera profile transform
-> as-shot WB + baseline exposure metadata
-> selectable base look
-> user Develop controls
-> detail/noise/sharpen recipe
-> output gamut map + transfer function
-> monitor transform (preview only)
```

### Công việc

1. Tách ba base look:
   - `Neutral`: chỉ technical profile + scene-to-display rolloff.
   - `iAi Natural`: S-curve/shoulder/toe nhẹ, bảo toàn hue/chroma và skin.
   - `Camera Match`: chỉ khi có legal profile/look hoặc user bật embedded-JPEG matching.
2. Embedded JPEG chỉ dùng cho thumbnail tức thời và optional Auto-Match. Không mặc định biến nó thành camera characterization khi đã có profile tốt.
3. Auto-Match nếu giữ lại phải là một recipe có confidence:
   - Tách exposure fit khỏi color fit.
   - Reject khi thumbnail crop/orientation khác, picture style cực đoan hoặc correspondence thấp.
   - Không fit 3×3/histogram nếu profile exact đang hoạt động, trừ opt-in.
4. Chuyển hard-coded taste constants thành versioned look parameters và camera/ISO-aware defaults.
5. Tone curve trong luminance/perceptual domain, giữ hue; highlight shoulder không desaturate sớm và toe không làm shadow bùn.
6. Dùng scene headroom thật cho highlight reconstruction/rolloff trước output clipping.
7. Version `raw_render_recipe`; file cũ giữ engine/look version cũ, file mới dùng recipe mới.

### Hoàn thành khi

- Default look mới được người dùng nghiệm thu trên contact sheet, không chỉ metric.
- Skin, foliage, sky và saturated highlights không có cast hệ thống.
- Embedded preview chuyển sang full render không “nhảy” sáng/màu khó chịu; nếu khác picture style thì UI giải thích look đang dùng.

---

## Quality Milestone Q5 — Kéo ánh sáng và màu giống workflow ART/Photoshop

### Thứ tự toán học bắt buộc

1. WB/CAT và exposure trong scene-linear working space.
2. Highlights/shadows/whites/blacks dựa trên exposure/luminance zones, không độc lập bóp từng RGB channel.
3. Tone mapping/curve bảo toàn hue trước creative color grading.
4. HSL/Color Mixer phân loại trong perceptual space phù hợp, có gamut awareness.
5. Saturation/vibrance bảo vệ skin, neutrals và màu đã bão hòa.
6. Local contrast/clarity/texture tách theo spatial scale và chủ yếu tác động luminance.
7. Noise reduction trước output sharpening; capture sharpening trước resize, output sharpening sau resize theo đích.
8. Gamut mapping và output encoding đúng một lần.

### Công việc

1. Với mỗi slider, viết contract: domain, pivot, range EV, vùng ảnh tác động, hue/luma invariants và clipping policy.
2. Exposure là nhân `2^EV`, giữ headroom; không dùng brightness gamma thay thế.
3. Highlights/whites dùng shoulder và mask exposure mềm; kéo âm phải khai thác channel còn dữ liệu, không chỉ chuyển xám.
4. Shadows/blacks có noise confidence để tránh nâng chroma noise trong vùng không có tín hiệu.
5. Color Mixer dùng một mô hình classification nhất quán giữa CPU/GPU; blend band trơn và giữ màu ngoài band gần identity.
6. Vibrance dựa trên saturation hiện tại và skin protection; saturation toàn cục chỉ là control trực tiếp có gamut compression.
7. Clarity/texture không được tạo halo lớn hoặc đổi saturation ngoài chủ ý.
8. Preview pointer-down có thể dùng proxy, nhưng release phải refine cùng exact pipeline; không được “nhảy look”.
9. Thêm slider-sweep golden và property tests: monotonic exposure, neutral preservation, bounded hue drift, finite output và CPU/GPU parity.

### Hoàn thành khi

- Kéo exposure ±2 EV, highlights/shadows và saturation trên corpus không tạo hue flip, banding hoặc clipping sớm hơn ART reference.
- Kết quả interactive và settled không đổi look nhìn thấy.
- Mỗi control có benchmark latency ở Fit và 100%.

---

## Quality Milestone Q6 — Detail, denoise và sharpening theo scale

### Công việc

1. Tách ba lớp sharpen:
   - Capture sharpen bù sensor/AA/demosaic.
   - Creative/detail sharpen do người dùng điều khiển.
   - Output sharpen theo kích thước/medium export.
2. Capture sharpen dùng edge/noise mask và camera/ISO metadata; không bake một gain cố định cho mọi RAW.
3. Đánh giá deconvolution tự viết hoặc thư viện license-compatible; ART RLD chỉ là reference behavior.
4. Chroma NR hoạt động trong không gian luma/chroma phù hợp, edge-aware và scale-aware; không làm bệt màu thật.
5. Luma NR bảo vệ texture; strength theo noise estimate/ISO nhưng user override luôn rõ ràng.
6. Preview Fit không được đánh giá sharpen bằng upscale proxy sai. UI cần 100% view cho quyết định detail chính xác.

### Hoàn thành khi

- Ảnh sắc hơn iAi baseline nhưng không có dark/bright halo hoặc waxy texture.
- Noise màu giảm mà edge màu thật không bleed.
- Export resize có output sharpen phù hợp và reproducible.

---

## Quality Milestone Q7 — Display/export color management và parity

### Công việc

1. Audit toàn đường:

```text
scene working RGB -> output linear RGB -> gamut map
-> output transfer/profile -> monitor ICC (preview only)
```

2. Xác nhận monitor profile được lấy đúng theo màn hình chứa cửa sổ và refresh khi đổi màn hình/profile.
3. Không áp monitor transform vào pixel export.
4. Export phải embed đúng ICC/output profile; untagged behavior được định nghĩa rõ.
5. Histogram/scopes lấy ở điểm pipeline được ghi nhãn: scene, pre-monitor display hoặc output; không dùng monitor-transformed pixels cho quyết định chỉnh sửa.
6. GPU LUT và CPU lcms path có parity tests trên sRGB, Display P3 và profile màn hình mẫu.
7. Thêm test phát hiện missing gamma, double gamma và double profile transform.

### Hoàn thành khi

- Preview trên màn hình chuẩn và file export mở trong color-managed viewer khớp trong tolerance.
- Chuyển màn hình không đổi file data; chỉ thay monitor appearance transform.
- Không còn khác biệt preview/commit/export do stage order hoặc profile bị áp hai lần.

---

## 7. Thứ tự triển khai khuyến nghị

Thứ tự thực tế để có hiệu quả sớm và giảm rủi ro, chia thành hai track có gate chung:

1. Memory M0 + Quality Q0 — cùng lập baseline memory và image quality trước mọi thay đổi.
2. Memory M1 — session nhẹ, single active working set; đây là bản sửa khẩn cấp.
3. Quality Q1 — normalized sensor master và technical/creative separation.
4. Memory M2 — bỏ duplicate buffers.
5. Quality Q2 + Memory M4 — tiled high-quality demosaic; giải quyết chất lượng và peak scratch cùng lúc.
6. Quality Q3 — profile system và camera coverage hợp pháp.
7. Quality Q4 — versioned default look, giảm phụ thuộc embedded-JPEG heuristic.
8. Memory M3 — batch tuần tự và spill.
9. Quality Q5/Q6 — slider behavior, detail, denoise và sharpen.
10. Memory M5 — viewport/crop preview dùng chính exact pipeline của quality track.
11. Quality Q7 — display/export color management parity.
12. Memory M6 — hợp nhất mọi cache/working set vào global budget manager.

Memory M1 là thay đổi đem lại mức giảm lớn nhất. Memory M4 giảm peak trong decode nhưng không thể tự giải quyết steady 12 GB. Memory M5 có giá trị lớn về UX nhưng khó hơn vì pipeline/filter cần hỗ trợ region processing.

Mỗi milestone nên là một chuỗi commit nhỏ, giữ build và test xanh. Không nên trộn refactor tile storage, session model và tiled AHD vào cùng một commit. Riêng Q2 và M4 dùng chung thiết kế tile/halo nhưng vẫn tách commit algorithm, memory layout và default selection để có thể bisect.

Q0 phải chạy trước khi tune look. Q3 phải có legal/provenance gate trước khi thêm bất kỳ profile asset nào. Q4 trở đi phải tăng `raw_render_recipe`/engine version nếu output mặc định thay đổi có chủ ý.

---

## 8. Ma trận kiểm thử bắt buộc

### 8.1. Functional

- Mở một RAW.
- Mở toàn bộ corpus.
- XMP lẫn trong selection.
- Chuyển tab tuần tự và ngẫu nhiên.
- Đổi develop parameters, chuyển tab rồi quay lại.
- Zoom fit, 100%, pan liên tục.
- Commit một RAW thành raster document.
- Thêm layer/selection/undo rồi spill và restore.
- Đóng tab active trong khi decode.
- Đóng ứng dụng khi batch đang chạy.
- Export một ảnh và batch toàn corpus.

### 8.2. Memory

- Current/peak RSS hoặc working set.
- Logical bytes theo memory class.
- Số full working sets.
- Scratch disk bytes.
- Mức giảm sau eviction/close.
- Ba vòng open/close để phát hiện tăng tích lũy.

### 8.3. Correctness

- Pixel comparison trước/sau refactor.
- Metadata/orientation/color profile.
- Bayer patterns và từng định dạng CR2/NEF/ARW/CR3/RAF.
- Undo/redo sau restore từ scratch.
- Không có tile seam trong AHD và filter có halo.

### 8.4. Chất lượng ảnh và color science

- Neutral và default-look renders của iAi/ART trên cùng RAW, WB, crop và output profile.
- ColorChecker Delta E 2000, skin hue, foliage, sky và saturated highlight.
- Crop 100% cho zipper, moire, false colour, hot/PDAF pixels, acutance, halo và noise.
- Highlight reconstruction khi một/hai channel clip.
- Slider sweeps cho exposure, highlights, shadows, whites, blacks, contrast, saturation, vibrance, HSL/Color Mixer, clarity và sharpen.
- Neutral invariance, bounded hue drift, monotonicity, finite-value và gamut tests.
- Preview GPU, settled CPU, commit, save/reopen và export parity.
- Monitor ICC, sRGB/Display P3 output, missing/double gamma và double-transform tests.
- Recipe-version compatibility: tài liệu cũ giữ look cũ; tài liệu mới dùng engine mới.
- Profile resolver provenance, exact camera matching, dual illuminant và fallback behavior.

### 8.5. Concurrency và lỗi

- Chuyển ảnh liên tục trong lúc preview đang render.
- Cancel batch.
- File bị xóa/đổi tên sau khi tạo entry.
- Scratch disk đầy hoặc không ghi được.
- Decode thất bại một ảnh không làm hỏng toàn phiên.
- Memory reservation bị từ chối.

---

## 9. Tiêu chí nghiệm thu cuối

Tính năng chỉ được coi là hoàn tất khi tất cả điều kiện sau đúng:

1. Corpus chuẩn mở thành công 20 RAW, XMP được xử lý đúng như file không hỗ trợ.
2. Sau khi idle, steady resident không quá 2,5 GiB trên máy đo hiện tại.
3. Peak luồng thông thường không quá 3,5 GiB sau khi tiled AHD hoàn thành.
4. Luôn chứng minh được số full RAW working set thường trú không vượt chính sách.
5. Batch RAM plateau và mọi output hợp lệ.
6. Chuyển tab không mất parameters, layer edits hoặc undo theo contract đã công bố.
7. Pixel output nằm trong tolerance; không có seam.
8. Không có tăng RAM tích lũy sau ba vòng open/close.
9. Scratch cleanup hoạt động sau đóng bình thường và sau crash giả lập.
10. Diagnostics đủ để giải thích một lần vượt budget nếu có.
11. Technical RAW master không chứa taste/look heuristic không versioned.
12. Camera profile đang dùng và fallback reason hiển thị được; không fuzzy-apply nhầm model.
13. Corpus có A/B report iAi–ART cho neutral/default look và crop 100%.
14. Có profile hợp lệ thì ColorChecker đạt median Delta E 2000 ≤ 3, P95 ≤ 6 hoặc có waiver theo camera với bằng chứng.
15. GPU preview/CPU settled/commit/export đạt parity đã định và không “nhảy look” sau khi thả slider.
16. Không có regression rõ rệt về zipper, false colour, halo, noise hoặc highlight hue so với ART reference.
17. Exposure và các control chính qua property/sweep tests; neutral không đổi hue ngoài tolerance.
18. Preview qua monitor ICC và export có profile đúng, không double gamma/profile.
19. Mọi DCP/ICC/profile được đóng gói có provenance và license được duyệt; không có asset sao chép từ ART.

Nếu Memory M1 đạt steady dưới 3 GiB nhưng chưa có tiled AHD, có thể phát hành như bản sửa khẩn cấp với peak cho phép tối đa 5 GiB và tiếp tục Memory M4 ngay sau đó.

---

## 10. Rủi ro và quyết định cần giữ

### Rủi ro kiến trúc

iAi là editor raster/layer tổng quát, còn ART chủ yếu là RAW developer không phá hủy. Không thể áp dụng máy móc mô hình của ART cho mọi tài liệu. Giải pháp là tách rõ pha RAW Develop khỏi `RasterDocument`; chỉ materialize khi công cụ yêu cầu.

### Rủi ro hiệu năng

Evict quá mạnh có thể làm chuyển tab chậm. Giải pháp là embedded preview tức thời, cache thumbnail, một prefetch có kiểm soát và scratch artifact tối ưu.

### Rủi ro dữ liệu

Không được evict tài liệu dirty trước khi parameters hoặc raster state được persist thành công. Scratch write phải atomic và có checksum/version.

### Rủi ro chất lượng ảnh

Tiled demosaic/filter có thể tạo seam nếu halo không đủ. Bắt buộc có golden comparison và seam-specific tests trước khi thay implementation mặc định.

Không được coi ART là “ground truth tuyệt đối”: default ART phụ thuộc profile và recipe đang chọn. Phải tách technical-neutral comparison khỏi pleasing-look comparison. Photoshop/ACR cũng là reference sản phẩm, không phải spec toán học.

Embedded JPEG là output đã qua picture style, denoise, sharpen và tone của camera. Dùng nó làm thumbnail hoặc optional Auto-Match được phép; dùng histogram/correspondence của nó như camera profile mặc định chỉ là fallback tạm thời và phải có confidence/failure diagnostics.

Tăng saturation/sharpen để ảnh “nịnh mắt” có thể che lỗi profile/demosaic trong một vài ảnh nhưng làm hỏng skin, shadow noise và highlight ở ảnh khác. Mọi tune phải qua corpus đa dạng và slider sweep.

### Quyết định không được đảo ngược tùy tiện

- Không mở N RAW thành N canvas full-resolution theo mặc định.
- Không dùng per-document memory budget độc lập.
- Không giữ RGBA8 mirror toàn ảnh chỉ vì một consumer có thể cần trong tương lai.
- Không chạy N full-resolution batch jobs song song.
- Không copy mã GPL từ ART vào dự án MIT.
- Không lấy DCP/ICC, `camconst.json`, `.arp`, LUT hoặc constants từ tree ART để phân phối trong iAi.
- Không bake creative look, color NR hoặc sharpen cố định vào technical RAW master mới.
- Không dùng embedded JPEG matching thay profile camera khi đã có characterization exact và người dùng không opt-in.
- Không thay đổi default RAW look mà không tăng recipe version và có golden/A-B report.

---

## 11. Handoff cho cuộc trò chuyện mới

Yêu cầu mở đầu đề xuất:

> Đọc toàn bộ `docs/planning/KE_HOACH_GIAM_RAM_MO_NHIEU_RAW_2026-08-25.md`, kiểm tra worktree hiện tại rồi bắt đầu Memory M0 và Quality Q0. Không sửa các thay đổi không liên quan. Trước Memory M1, hãy đưa ra baseline memory report từ corpus `C:\Users\Admin\Pictures\anh-raw`. Trước khi tune màu/độ nét, hãy tạo A/B baseline iAi–ART, audit profile resolution của từng camera và tách technical-neutral khỏi default-look comparison. ART chỉ là reference/black-box oracle; tuyệt đối không copy code hoặc asset GPL.

Việc đầu tiên của agent mới:

1. Đọc tài liệu này hoàn chỉnh.
2. Chạy `git status --short`; bảo toàn mọi thay đổi sẵn có.
3. Đọc lại các file được nêu trong mục 3.2 vì line number có thể thay đổi.
4. Đọc thêm `docs/planning/KE_HOACH_COLOR_LIGHT_PIPELINE_2026-08-09.md` và `docs/planning/HANDOFF_PHASE6_LARGE_RAW_PERFORMANCE_2026-08-09.md` như lịch sử kỹ thuật; nếu mâu thuẫn, tài liệu hiện tại là handoff mới hơn.
5. Chạy release corpus probe để xác nhận môi trường.
6. Lập baseline theo memory class trước khi thay đổi ownership.
7. Chạy `raw_look_probe`, DCP/color reference và CPU/GPU parity tests hiện có.
8. Dùng ART reference commit/config cố định để tạo neutral/default/slider A/B; không lấy profile ART đưa vào iAi.
9. Triển khai Memory M0 và Quality Q0, báo số liệu/bảng crop trước khi sửa behavior.
10. Sau baseline, ưu tiên Memory M1 để chặn lỗi 12 GB; sau đó đi theo thứ tự mục 7.

Tại thời điểm cập nhật tài liệu, chưa có mã nguồn nào được sửa để xử lý lỗi RAM hoặc chất lượng. Worktree đã có file không liên quan `docs/planning/KE_HOACH_ARTBOARD_DA_TRANG_2026-08-22.md` ở trạng thái untracked; phải bảo toàn file đó. Tài liệu kế hoạch hiện tại cũng đang untracked cho đến khi người dùng chủ động commit.

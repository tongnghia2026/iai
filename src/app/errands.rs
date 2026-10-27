//! "Nhờ AI làm": the person picks a job and a folder; iAi writes what to
//! ask, holds it as an errand until the relay Antigravity runs takes it
//! (see `crate::antigravity`), and follows the agent's work on the window:
//! each command it runs, its task's end. "Dừng" refuses its next calls.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use iai_automation::Grants;
use iai_automation::bridge::{Errand, Op, Request};
use iai_engine::{CmdError, ErrorCode};
use serde_json::Value;

use super::state::App;
use crate::ui::{AntigravityState, ErrandForm, ErrandIntent, ErrandJob, ErrandRunView, ErrandView};

/// How long the relay may be unheard before Antigravity is started.
const RELAY_WAIT: Duration = Duration::from_secs(6);
/// How long an errand may wait for the relay to take it.
const TAKE_WAIT: Duration = Duration::from_secs(90);
/// How long the agent may be silent before the errand is given up.
const QUIET: Duration = Duration::from_secs(20 * 60);
/// How long "Dừng" holds the agent off.
const STOP_HOLD: Duration = Duration::from_secs(10 * 60);
/// How often Antigravity's setup is read again while the window is open.
const SETUP_FRESH: Duration = Duration::from_secs(5);
/// The results' folder inside the customer's.
pub(crate) const RESULTS: &str = "iAi_ket_qua";
/// The longest note taken from the person.
const MAX_NOTE: usize = 2000;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Stage {
    /// Held for the relay.
    Waiting,
    /// The relay handed it on: a conversation in Antigravity.
    Sent,
    /// The agent began its task.
    Working,
    Done,
    Failed(String),
    Stopped,
}

impl Stage {
    fn running(&self) -> bool {
        matches!(self, Stage::Waiting | Stage::Sent | Stage::Working)
    }
}

/// The errand under way, or the last one.
pub(crate) struct Under {
    pub(crate) id: u64,
    pub(crate) title: String,
    pub(crate) folder: PathBuf,
    pub(crate) stage: Stage,
    pub(crate) steps: u32,
    pub(crate) last: String,
    pub(crate) since: Instant,
    /// The last word from the relay or the agent.
    pub(crate) heard: Instant,
}

/// "Nhờ AI làm"'s state in the app.
#[derive(Default)]
pub(crate) struct Errands {
    pub(crate) open: bool,
    /// The folder of photos chosen.
    pub(crate) folder: Option<PathBuf>,
    next: u64,
    waiting: VecDeque<Errand>,
    pub(crate) under: Option<Under>,
    relay_seen: Option<Instant>,
    /// What the person granted agents: the folders errands were about (to
    /// read) and their results' folders (to write).
    granted: Grants,
    stopped_until: Option<Instant>,
    launched: bool,
    /// Antigravity's setup as last read, and when.
    setup: Option<(Instant, AntigravityState)>,
    connect_note: Option<String>,
    /// The job asked for from elsewhere, counted (see [`ErrandView::preset`]).
    preset: Option<(u64, ErrandJob)>,
}

/// Online AI may draw this errand's hard cases (a photo to edit is all
/// online AI's).
fn online(form: &ErrandForm) -> bool {
    form.job.on_open_photo() || (form.online && form.job.online_ok())
}

/// The photo "Sửa ảnh đang mở" works on.
pub(crate) struct Photo {
    /// Its tab's document in the window.
    pub(crate) doc: u32,
    pub(crate) title: String,
    /// The copy sent to the AI: the tab as it looked when the person sent it.
    pub(crate) sent: PathBuf,
    /// Where the AI's picture is kept, in the results' folder.
    pub(crate) keep: PathBuf,
    /// The frame nearest the photo's that the image generator draws.
    pub(crate) aspect: &'static str,
}

/// The image generator's frame nearest `width` × `height`.
pub(crate) fn aspect(width: u32, height: u32) -> &'static str {
    const FRAMES: [(&str, f64); 9] = [
        ("1:1", 1.0),
        ("2:3", 2.0 / 3.0),
        ("3:2", 1.5),
        ("3:4", 0.75),
        ("4:3", 4.0 / 3.0),
        ("4:5", 0.8),
        ("5:4", 1.25),
        ("9:16", 0.5625),
        ("16:9", 16.0 / 9.0),
    ];
    let ratio = f64::from(width.max(1)) / f64::from(height.max(1));
    FRAMES
        .iter()
        .min_by(|a, b| {
            let off = |f: f64| (ratio.ln() - f.ln()).abs();
            off(a.1).total_cmp(&off(b.1))
        })
        .map_or("1:1", |f| f.0)
}

/// The clothes online AI is to put on, in the agent's words; none to keep
/// them.
fn shirt(form: &ErrandForm) -> Option<&'static str> {
    if !online(form) || !matches!(form.job, ErrandJob::IdPhotos | ErrandJob::DressIdPhoto) {
        return None;
    }
    match form.shirt.as_str() {
        "white_shirt" => Some("áo sơ mi trắng cổ bẻ, cài cúc, phẳng phiu"),
        "vest" => Some("áo vest đen, bên trong sơ mi trắng"),
        "ao_dai" => Some("áo dài trắng truyền thống"),
        _ => None,
    }
}

/// The job as one line, for the agent and the conversation's title.
fn job_line(form: &ErrandForm) -> String {
    let backdrop = if form.backdrop == "blue" {
        "xanh"
    } else {
        "trắng"
    };
    let dressed = shirt(form)
        .map(|s| format!(" Mọi người mặc {s} (trừ khi lời dặn nói khác)."))
        .unwrap_or_default();
    match form.job {
        ErrandJob::IdCards => "In căn cước của khách lên giấy A4.".to_string(),
        ErrandJob::IdPhotos => format!(
            "Làm ảnh thẻ {} nền {backdrop} cho từng người, mỗi người {} tấm (trừ khi lời dặn \
             nói khác), xếp lên giấy ảnh {}.{dressed}",
            form.size, form.copies, form.paper
        ),
        ErrandJob::DressIdPhoto => match shirt(form) {
            Some(clothes) => format!(
                "Thay áo cho ảnh thẻ đang mở trong iAi: mặc {clothes}, vẽ đủ phần vai bị thiếu; \
                 giữ nguyên khuôn mặt, tóc và bố cục."
            ),
            None => {
                "Lấp phần vai, thân bị thiếu của ảnh thẻ đang mở trong iAi, giữ nguyên áo khách \
                     đang mặc, khuôn mặt, tóc và bố cục."
                    .to_string()
            }
        },
        ErrandJob::ScanCleanup => {
            "Làm sạch các ảnh chụp giấy tờ, văn bản cho rõ chữ, trắng nền để in.".to_string()
        }
        ErrandJob::EditPhoto => format!(
            "Sửa ảnh đang mở trong iAi bằng AI vẽ, theo lời dặn: {}",
            form.note.trim().chars().take(MAX_NOTE).collect::<String>()
        ),
        ErrandJob::Other => form.note.trim().chars().take(MAX_NOTE).collect(),
    }
}

/// How the photo open in the window is edited: drawn anew, back as a new
/// layer of its tab, a copy kept in the results' folder.
fn photo_steps(photo: &Photo) -> String {
    format!(
        "\
Ảnh gửi AI: {sent} (bản chụp tab «{title}» lúc người dùng bấm gửi)
Tab gốc trong iAi: doc {doc}

Cách làm:
1. Gọi invoke_subagent với TypeName \"image-generator\". Trong Prompt: đường dẫn ảnh gửi AI ở trên \
(để nó đưa vào ImagePaths), lời dặn ở dòng «Việc», và dặn giữ nguyên mọi thứ khác — khuôn mặt, \
tóc, dáng người, bố cục, góc chụp, vị trí và cỡ các vật; khung {aspect}; không thêm chữ.
2. Đặt ảnh vẽ xong thành LỚP MỚI trong tab gốc: iai_layer_place với {{\"doc\": {doc}, \"from\": \
{{\"path\": \"<đường dẫn ảnh vẽ xong>\"}}, \"fit\": \"cover\", \"name\": \"AI: <vài chữ về việc đã \
sửa>\"}}. Không sửa, không xóa, không gộp các lớp khác, không làm phẳng ảnh, không mở tab mới cho \
việc này. iAi báo người dùng đang thao tác (Busy) thì chờ vài giây rồi gọi lại.
3. Lưu một bản ảnh vẽ xong vào thư mục kết quả: iai_file_open ảnh đó, rồi iai_command_run \
file.export {{\"doc\": <số tab vừa mở>, \"path\": \"{keep}\"}}, rồi doc.close {{\"doc\": <số đó>, \
\"discard\": true}}.
4. Kiểm bằng iai_render_preview {{\"doc\": {doc}}}, rồi gọi iai_task_end.
- Câu trả lời cuối: đã sửa gì, lớp mới tên gì, file lưu ở đâu. Nhắc người dùng có thể vẽ mặt nạ \
(mask) trên lớp mới để giữ lại phần nào của ảnh gốc, và so khuôn mặt trước khi in.
",
        sent = photo.sent.display(),
        title = photo.title,
        doc = photo.doc,
        aspect = photo.aspect,
        keep = photo.keep.display(),
    )
}

/// How the ID photo open in the window is dressed: the AI draws on the
/// photo with its missing part grey; `id_photo.dress` lays the drawing back
/// face on face, the photo's own face, hair and backdrop kept.
fn dress_steps(photo: &Photo, form: &ErrandForm) -> String {
    let (what, keep_clothes) = match shirt(form) {
        Some(clothes) => (
            format!(
                "Mặc cho người trong ảnh {clothes}, vẽ cả phần vai và thân bị thiếu (vùng màu \
                 xám) cho đủ, hai vai cân đối."
            ),
            false,
        ),
        None => (
            "Vẽ tiếp phần vai, thân và áo vào vùng màu xám cho liền với áo người trong ảnh đang \
             mặc (cùng kiểu, cùng màu, cùng nếp); phần không xám giữ nguyên."
                .to_string(),
            true,
        ),
    };
    format!(
        "\
Ảnh gửi AI: {sent} (ảnh thẻ trong tab «{title}»; vùng XÁM là phần ảnh khách chụp không có)
Tab ảnh thẻ trong iAi: doc {doc}

Cách làm:
1. Gọi invoke_subagent với TypeName \"image-generator\". Trong Prompt: đường dẫn ảnh gửi AI ở trên \
(để nó đưa vào ImagePaths) và dặn đúng như sau: «Ảnh thẻ chân dung. {what} GIỮ NGUYÊN khuôn mặt, tóc \
và cả cái đầu: cùng vị trí, cùng cỡ, cùng góc nghiêng — không dời, không phóng to thu nhỏ, không \
xoay, không làm đẹp hay sửa mặt. Giữ nguyên khung hình và bố cục như ảnh gửi. Nền trơn cùng màu \
nền ảnh gửi. Khung {aspect}. Không thêm chữ.»
2. Đặt ảnh vẽ xong lên ảnh thẻ: iai_command_run id_photo.dress {{\"doc\": {doc}, \"path\": \
\"<đường dẫn ảnh vẽ xong>\", \"keepClothes\": {keep_clothes}}}. Lệnh tự xoay, co giãn ảnh AI cho \
mặt khớp đúng mặt ảnh gốc, giữ mặt, tóc, nền của ảnh gốc và thêm một lớp mới có mặt nạ. KHÔNG \
dùng iai_layer_place cho việc này. Không sửa, không xóa, không gộp các lớp khác, không làm phẳng \
ảnh. iAi báo người dùng đang thao tác (Busy) thì chờ vài giây rồi gọi lại.
3. Lưu một bản ảnh vẽ xong vào thư mục kết quả: iai_file_open ảnh đó, rồi iai_command_run \
file.export {{\"doc\": <số tab vừa mở>, \"path\": \"{keep}\"}}, rồi doc.close {{\"doc\": <số đó>, \
\"discard\": true}}.
4. Kiểm bằng iai_render_preview {{\"doc\": {doc}}}. Nếu bước 2 báo «mặt AI vẽ lệch dáng», hoặc ảnh \
vẽ dời đầu, đổi cỡ đầu, thì nhờ vẽ lại MỘT lần (dặn kỹ hơn giữ nguyên đầu) rồi làm lại bước 2; lớp \
cũ để nguyên cho người dùng chọn. Rồi gọi iai_task_end.
- Câu trả lời cuối: đã làm gì, lớp mới tên gì, file lưu ở đâu. Nhắc người dùng xem chỗ cổ áo và \
vai; chỗ nào chưa ưng thì tô mặt nạ của lớp mới.
",
        sent = photo.sent.display(),
        title = photo.title,
        doc = photo.doc,
        aspect = photo.aspect,
        keep = photo.keep.display(),
    )
}

/// How the job is done, when no saved skill does it.
fn job_steps(form: &ErrandForm) -> String {
    match form.job {
        ErrandJob::IdCards => "\
1. Xem mọi ảnh bằng iai_file_contact_sheet. Phân ra: mặt trước, mặt sau thẻ căn cước chụp bằng \
máy ảnh (thường nghiêng, có nền bàn); ảnh chụp màn hình căn cước điện tử (VNeID) trên điện \
thoại; ảnh nào của ai (ảnh gửi cùng lúc thường có tên file bắt đầu bằng cùng một dãy số; ảnh nhỏ \
khó đọc thì xem lại riêng bằng iai_file_contact_sheet với cellPx lớn để đọc họ tên, đừng đoán). \
Bỏ qua ảnh không phải căn cước.
2. Xếp bằng print.card_sheet: mỗi thẻ một hàng gồm mặt trước và mặt sau; ảnh chụp màn hình căn \
cước điện tử là một hàng riêng, xoay nằm ngang, không cắt. Lệnh tự tìm bốn góc thẻ chụp nghiêng \
và cắt thẳng đúng cỡ thật; mục nào báo «unsure» thì xem ảnh đó rồi đưa «corners».
3. Xuất PDF «can_cuoc.pdf» vào thư mục kết quả."
            .to_string(),
        ErrandJob::IdPhotos => match shirt(form) {
            None => format!(
                "\
1. Xem ảnh bằng iai_file_contact_sheet; mỗi ảnh chân dung là một người (bỏ qua ảnh căn cước, \
giấy tờ).
2. Làm bằng id_photo.batch: cỡ {}, nền {}, giấy {}, mỗi người đúng số tấm.
3. Xuất PDF «anh_the.pdf» vào thư mục kết quả. Ai không thấy rõ mặt thì nói ở câu trả lời cuối.",
                form.size, form.backdrop, form.paper
            ),
            Some(clothes) => format!(
                "\
1. Xem ảnh bằng iai_file_contact_sheet; mỗi ảnh chân dung là một người (bỏ qua ảnh căn cước, \
giấy tờ).
2. Mỗi ảnh chân dung: nhờ image-generator mặc {clothes}, khung 3:4, nền trơn sáng.
3. Làm bằng id_photo.batch trên các ẢNH ĐÃ THAY ÁO (không phải ảnh gốc): cỡ {}, nền {}, giấy {}, \
mỗi người đúng số tấm. Nếu chạy kỹ năng đã lưu thì cũng đưa ảnh đã thay áo vào thay ảnh gốc.
4. Xuất PDF «anh_the.pdf» vào thư mục kết quả. Ai không thấy rõ mặt thì nói ở câu trả lời cuối.",
                form.size, form.backdrop, form.paper
            ),
        },
        ErrandJob::ScanCleanup => "\
1. Xem ảnh bằng iai_file_contact_sheet; chỉ làm những ảnh chụp giấy tờ, văn bản.
2. Mỗi ảnh: mở, trang chụp nghiêng thì cắt thẳng bằng image.perspective_crop, làm sạch bằng \
image.scan_cleanup, lưu JPG cùng tên vào thư mục kết quả."
            .to_string(),
        ErrandJob::DressIdPhoto | ErrandJob::EditPhoto | ErrandJob::Other => String::new(),
    }
}

/// What online AI may do, or that it may not: iAi's own tools come first.
/// `images` is where the drawn pictures land (iAi may read there).
fn ai_rules(form: &ErrandForm, images: Option<&Path>) -> String {
    let (true, Some(images)) = (online(form), images) else {
        return "- Không dùng trợ lý vẽ ảnh (image-generator, generate_image): mọi việc ảnh làm \
                bằng công cụ iai."
            .to_string();
    };
    if form.job.on_open_photo() {
        return format!(
            "\
- Ảnh vẽ xong nằm trong thư mục {images} (iAi được phép đọc ở đó).
- Nhờ vẽ tối đa 2 lần; báo hết lượt (quota) thì dừng và nói lại.
- Không được nói \"giữ nguyên 100%\": mô hình vẽ lại cả ảnh.",
            images = images.display()
        );
    }
    let cases = match shirt(form) {
        Some(clothes) => format!("thay áo ({clothes}), "),
        None => "thay áo, ".to_string(),
    };
    format!(
        "\
- AI online cho ca khó (người dùng đã bật): chỉ dùng cho ảnh thật cần — {cases}xóa chữ hay vật \
che mặt, nền quá rối mà công cụ iai làm không sạch. Ảnh khác làm bằng công cụ iai như thường.
- Cách gọi: invoke_subagent với TypeName \"image-generator\"; trong Prompt đưa đường dẫn ảnh gốc \
(để nó đưa vào ImagePaths) và dặn: giữ nguyên khuôn mặt, tóc, dáng đầu, góc chụp; chỉ đổi phần \
được yêu cầu; không thêm chữ.
- Ảnh vẽ xong nằm trong thư mục {images} (iAi được phép đọc ở đó): mở bằng iai_file_open hoặc đưa \
đường dẫn đó vào lệnh iai, làm tiếp như ảnh thường. MỌI ảnh vẽ xong đều phải lưu một bản vào thư \
mục kết quả (iai_file_open rồi file.export), đừng để nó chỉ nằm trong thư mục của Antigravity.
- Mỗi ảnh nhờ vẽ tối đa 2 lần; báo hết lượt (quota) thì làm bằng công cụ iai và nói lại.
- Câu trả lời cuối: kể rõ ảnh nào do AI vẽ lại và nhắc người dùng so khuôn mặt với ảnh gốc trước \
khi in; không được nói \"giữ nguyên 100%\".",
        images = images.display()
    )
}

/// What the agent is asked for `form` on `folder`: the conversation's
/// title, and the ask in full. `images` is where online AI's pictures land;
/// `photo`, the photo open in the window when that is the job.
pub(crate) fn ask(
    form: &ErrandForm,
    folder: &Path,
    images: Option<&Path>,
    photo: Option<&Photo>,
) -> (String, String) {
    // With nowhere to read its pictures from, online AI is of no use.
    let offline;
    let form = if images.is_none() && form.online {
        offline = ErrandForm {
            online: false,
            ..form.clone()
        };
        &offline
    } else {
        form
    };
    let job = job_line(form);
    let title_job = match form.job {
        ErrandJob::Other => "Việc khác",
        ErrandJob::EditPhoto => "Sửa ảnh",
        ErrandJob::DressIdPhoto => "Thay áo ảnh thẻ",
        other => other.label(),
    };
    let about = match photo {
        Some(p) if form.job.on_open_photo() => p.title.clone(),
        _ => folder
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default(),
    };
    let title = format!("iAi · {title_job} · {about}");
    let results = folder.join(RESULTS);
    let steps = match photo {
        Some(p) if form.job == ErrandJob::DressIdPhoto => format!("\n{}", dress_steps(p, form)),
        Some(p) if form.job.on_open_photo() => format!("\n{}", photo_steps(p)),
        _ => {
            let steps = job_steps(form);
            if steps.is_empty() {
                String::new()
            } else {
                format!("\nCách làm gợi ý (khi không có kỹ năng đã lưu làm việc này):\n{steps}\n")
            }
        }
    };
    let note = if matches!(form.job, ErrandJob::Other | ErrandJob::EditPhoto) {
        String::new()
    } else {
        let n: String = form.note.trim().chars().take(MAX_NOTE).collect();
        if n.is_empty() {
            String::new()
        } else {
            format!("\nNgười dùng dặn thêm: {n}\n")
        }
    };
    let prompt = format!(
        "\
Đây là việc người dùng bấm gửi từ nút «Nhờ AI làm» trong app iAi. Họ đang nhìn màn hình iAi, \
không ngồi trả lời câu hỏi, nên đừng hỏi lại: chỗ nào không chắc thì chọn cách hợp lý nhất và \
nói rõ ở câu trả lời cuối.

Việc: {job}
Thư mục ảnh của khách: {folder}
Thư mục kết quả: {results}
{note}
Quy tắc:
- Chỉ dùng các công cụ MCP của máy chủ «iai». Không dùng terminal, không viết chương trình riêng.
- Mở đầu bằng iai_task_begin với request là dòng «Việc» ở trên. Nếu nó gợi ý một kỹ năng đã lưu \
hợp với việc này thì chạy kỹ năng đó bằng iai_skill_run (xem tham số bằng iai_skill_get).
- Chỉ ghi vào thư mục kết quả. Không sửa, không xóa, không đổi tên ảnh gốc.
- Ảnh mở ra chỉ để xem hay để làm thì đóng lại khi xong (doc.close với discard: true); để lại \
các tab kết quả mở sẵn cho người dùng xem.
- Xong: kiểm bằng iai_render_preview, rồi gọi iai_task_end (status \"done\" khi đã đúng; \
\"failed\" khi không làm được).
- Câu trả lời cuối: vài dòng tiếng Việt, nói đã làm gì, file kết quả ở đâu, chỗ nào người dùng \
cần xem lại.
{ai}
{steps}",
        folder = folder.display(),
        results = results.display(),
        ai = ai_rules(form, images),
    );
    (title, prompt)
}

fn stopped_error() -> CmdError {
    CmdError::new(
        ErrorCode::PermissionDenied,
        "the person pressed Stop (Dừng): stop this task now, do not try again",
    )
    .vi("Người dùng đã bấm Dừng")
}

impl App {
    /// "Nhờ AI làm" as its window shows it.
    pub(crate) fn errand_view(&self) -> ErrandView {
        let e = &self.shell.errands;
        ErrandView {
            open: e.open,
            antigravity: e.setup.map(|(_, s)| s).unwrap_or_default(),
            connect_note: e.connect_note.clone(),
            folder: e
                .folder
                .as_ref()
                .map(|f| f.display().to_string())
                .unwrap_or_default(),
            photo: {
                let doc = self.docs.active();
                (!self.shell.ui.show_welcome && !doc.is_flow_text())
                    .then(|| doc.tab_title().to_string())
            },
            pro: self.pro_open(),
            preset: e.preset,
            run: e.under.as_ref().map(|u| {
                let stage = match &u.stage {
                    Stage::Waiting if e.launched => "Đang mở Antigravity…".to_string(),
                    Stage::Waiting => "Đang chuyển việc sang Antigravity…".to_string(),
                    Stage::Sent => "Gemini đã nhận việc, đang đọc yêu cầu…".to_string(),
                    Stage::Working => "Gemini đang làm trên iAi".to_string(),
                    Stage::Done => "Xong. Xem lại kết quả trước khi in.".to_string(),
                    Stage::Failed(why) => why.clone(),
                    Stage::Stopped => "Đã dừng.".to_string(),
                };
                let results = u.folder.join(RESULTS);
                ErrandRunView {
                    title: u.title.clone(),
                    stage,
                    steps: u.steps,
                    last: u.last.clone(),
                    running: u.stage.running(),
                    failed: matches!(u.stage, Stage::Failed(_)),
                    results: results.is_dir().then(|| results.display().to_string()),
                }
            }),
        }
    }

    pub(in crate::app) fn pro_open(&self) -> bool {
        #[cfg(feature = "pro")]
        {
            self.shell.pro.status().open()
        }
        #[cfg(not(feature = "pro"))]
        false
    }

    /// The tools Antigravity is told to let run.
    fn agent_tools(&self) -> Vec<String> {
        iai_automation::mcp::tool_names(&self.engine.registry().list())
    }

    /// Read Antigravity's setup again when it is old (the window is open).
    fn read_setup(&mut self, now: bool) {
        let e = &self.shell.errands;
        if !now && e.setup.is_some_and(|(at, _)| at.elapsed() < SETUP_FRESH) {
            return;
        }
        let state = match crate::antigravity::Antigravity::at_home() {
            Some(agy) if agy.installed() => {
                let exe = std::env::current_exe().unwrap_or_default();
                if agy.missing(&exe, &self.agent_tools()).is_empty() {
                    AntigravityState::Ready
                } else {
                    AntigravityState::NotConnected
                }
            }
            _ => AntigravityState::Absent,
        };
        self.shell.errands.setup = Some((Instant::now(), state));
    }

    pub(crate) fn errand_intent(&mut self, intent: ErrandIntent) {
        match intent {
            ErrandIntent::Open => {
                self.shell.errands.open = true;
                self.read_setup(true);
            }
            ErrandIntent::OpenFor(job) => {
                let e = &mut self.shell.errands;
                e.open = true;
                let n = e.preset.map_or(1, |(n, _)| n.wrapping_add(1));
                e.preset = Some((n, job));
                self.read_setup(true);
            }
            ErrandIntent::Close => self.shell.errands.open = false,
            ErrandIntent::Connect => self.connect_antigravity(),
            ErrandIntent::PickFolder => self.pick_errand_folder(),
            ErrandIntent::Send(form) => self.send_errand(&form),
            ErrandIntent::Stop => self.stop_errand(),
            ErrandIntent::OpenResults => {
                if let Some(u) = &self.shell.errands.under {
                    let dir = u.folder.join(RESULTS);
                    #[cfg(windows)]
                    let _ = std::process::Command::new("explorer.exe").arg(&dir).spawn();
                    let _ = dir;
                }
            }
        }
    }

    fn connect_antigravity(&mut self) {
        let Some(agy) = crate::antigravity::Antigravity::at_home() else {
            return;
        };
        let exe = std::env::current_exe().unwrap_or_default();
        let downloads = std::env::var_os("USERPROFILE")
            .map(|h| PathBuf::from(h).join("Downloads"))
            .unwrap_or_default();
        let images = crate::antigravity::images_folder();
        let note = match agy.connect(&exe, &self.agent_tools(), &downloads, images.as_deref()) {
            Ok(true) => {
                crate::diag::note("errand", "connected to Antigravity; it starts again");
                if crate::antigravity::startable() {
                    std::thread::spawn(crate::antigravity::restart);
                    "Đã nối. Antigravity đang khởi động lại để nhận cài đặt mới.".to_string()
                } else {
                    "Đã nối. Tắt hẳn Antigravity rồi mở lại một lần để nó nhận cài đặt mới."
                        .to_string()
                }
            }
            Ok(false) => "Đã nối với Antigravity.".to_string(),
            Err(why) => why,
        };
        self.shell.errands.connect_note = Some(note);
        self.read_setup(true);
    }

    fn pick_errand_folder(&mut self) {
        let Some(window) = self.win.window.as_ref() else {
            return;
        };
        let parent = crate::file_io::dialog_parent(window);
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let title = "Chọn thư mục ảnh của khách";
            if let Some(dir) = crate::file_io::dialog_pick_folder(parent, title) {
                let _ = tx.send(crate::file_io::FileDialogResult::PickedErrandFolder(dir));
            }
        });
        self.jobs.pending_file_dialog = Some(rx);
        window.request_redraw();
    }

    /// Hold `form` as an errand for the relay; let agents into the window
    /// (the person asked for one to work here) and into the folder.
    /// The folder of the photo open in the window (its file's, else the one
    /// chosen, else Downloads); `None` when no photo is open.
    fn open_photo_folder(&self) -> Option<PathBuf> {
        let doc = self.docs.active();
        if self.shell.ui.show_welcome || doc.is_flow_text() {
            return None;
        }
        doc.path
            .as_ref()
            .and_then(|p| p.parent())
            .map(Path::to_path_buf)
            .filter(|d| d.is_dir())
            .or_else(|| self.shell.errands.folder.clone())
            .or_else(|| std::env::var_os("USERPROFILE").map(|h| PathBuf::from(h).join("Downloads")))
    }

    /// The photo open in the window as the AI is sent it: its page flattened
    /// onto white, at most 2048 pixels long, a JPEG in `results`; an ID photo
    /// to dress, with what the customer's photo missed in grey.
    fn photo_to_send(&mut self, results: &Path, job: ErrandJob) -> Result<Photo, String> {
        let photo = self.flat_photo_to_send(results)?;
        if job == ErrandJob::DressIdPhoto {
            let call = iai_engine::CommandCall::new(
                "id_photo.dress_input",
                serde_json::json!({"doc": photo.doc, "path": photo.sent.display().to_string()}),
            );
            self.execute_command(call)
                .map_err(|e| super::engine::error_line(&e))?;
        }
        Ok(photo)
    }

    fn flat_photo_to_send(&self, results: &Path) -> Result<Photo, String> {
        let doc = self.docs.active();
        let stem: String = doc
            .path
            .as_ref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| doc.tab_title().to_string())
            .chars()
            .map(|c| if "\\/:*?\"<>|".contains(c) { '_' } else { c })
            .collect();
        let (w, h) = (doc.canvas.width, doc.canvas.height);
        let mut rgba = doc.canvas.flatten_for_export();
        let long = w.max(h).max(1);
        let (sw, sh) = if long > 2048 {
            let scale = 2048.0 / f64::from(long);
            (
                ((f64::from(w) * scale).round() as u32).max(1),
                ((f64::from(h) * scale).round() as u32).max(1),
            )
        } else {
            (w, h)
        };
        if (sw, sh) != (w, h) {
            rgba = crate::core::canvas::downscale_rgba(&rgba, w, h, sw, sh);
        }
        let rgb: Vec<u8> = rgba
            .chunks_exact(4)
            .flat_map(|p| {
                let a = u32::from(p[3]);
                let on_white = |c: u8| ((u32::from(c) * a + 255 * (255 - a)) / 255) as u8;
                [on_white(p[0]), on_white(p[1]), on_white(p[2])]
            })
            .collect();
        let free = |name: &str| {
            let first = results.join(format!("{name}.jpg"));
            (2..100)
                .map(|n| results.join(format!("{name}_{n}.jpg")))
                .fold(
                    first,
                    |found, next| if found.exists() { next } else { found },
                )
        };
        let sent = free(&format!("{stem}_gui_AI"));
        let image = image::RgbImage::from_raw(sw, sh, rgb)
            .ok_or_else(|| "Ảnh đang mở không đọc được".to_string())?;
        image
            .save_with_format(&sent, image::ImageFormat::Jpeg)
            .map_err(|e| format!("Không lưu được ảnh gửi AI: {e}"))?;
        Ok(Photo {
            doc: doc.id.0,
            title: doc.tab_title().to_string(),
            sent,
            keep: free(&format!("{stem}_AI")),
            aspect: aspect(w, h),
        })
    }

    pub(crate) fn send_errand(&mut self, form: &ErrandForm) {
        if self
            .shell
            .errands
            .under
            .as_ref()
            .is_some_and(|u| u.stage.running())
        {
            return;
        }
        let folder = if form.job.on_open_photo() {
            let Some(folder) = self.open_photo_folder() else {
                self.shell.status_msg = "Mở ảnh cần sửa trước rồi gửi lại".to_string();
                return;
            };
            folder
        } else {
            let Some(folder) = self.shell.errands.folder.clone() else {
                return;
            };
            folder
        };
        if !folder.is_dir() {
            return;
        }
        let results = folder.join(RESULTS);
        if let Err(e) = std::fs::create_dir_all(&results) {
            self.shell.status_msg = format!("Không tạo được thư mục kết quả: {e}");
            return;
        }
        // Online AI's pictures are read where Antigravity puts them.
        let images = online(form)
            .then(crate::antigravity::images_folder)
            .flatten();
        let photo = if form.job.on_open_photo() {
            if images.is_none() {
                self.shell.status_msg = "Máy này chưa có Antigravity để AI vẽ ảnh".to_string();
                return;
            }
            match self.photo_to_send(&results, form.job) {
                Ok(p) => Some(p),
                Err(why) => {
                    self.shell.status_msg = why;
                    return;
                }
            }
        } else {
            None
        };
        self.set_agent_control(true);
        if !self.agent_control_on() {
            return;
        }
        let (title, prompt) = ask(form, &folder, images.as_deref(), photo.as_ref());
        let e = &mut self.shell.errands;
        if let Some(images) = images
            && !e.granted.read.contains(&images)
        {
            e.granted.read.push(images);
        }
        e.next = e.next.saturating_add(1);
        let id = e.next;
        e.waiting.clear();
        e.waiting.push_back(Errand {
            id,
            title: title.clone(),
            prompt,
            model: None,
        });
        if !e.granted.read.contains(&folder) {
            e.granted.read.push(folder.clone());
        }
        if !e.granted.write.contains(&results) {
            e.granted.write.push(results);
        }
        e.stopped_until = None;
        e.launched = false;
        let now = Instant::now();
        e.under = Some(Under {
            id,
            title: title.clone(),
            folder,
            stage: Stage::Waiting,
            steps: 0,
            last: String::new(),
            since: now,
            heard: now,
        });
        crate::diag::note("errand", &format!("{id} sent: {title}"));
    }

    fn stop_errand(&mut self) {
        let e = &mut self.shell.errands;
        e.waiting.clear();
        e.stopped_until = Instant::now().checked_add(STOP_HOLD);
        if let Some(u) = e.under.as_mut()
            && u.stage.running()
        {
            u.stage = Stage::Stopped;
            crate::diag::note("errand", &format!("{} stopped by the person", u.id));
        }
    }

    /// Before a command from an agent: refused after "Dừng"; else counted
    /// as a step of the errand under way.
    pub(crate) fn errand_step(&mut self, op: &Op) -> Option<CmdError> {
        let changes = matches!(op, Op::Execute { .. } | Op::Batch { .. });
        if !changes {
            return None;
        }
        let e = &mut self.shell.errands;
        if e.stopped_until.is_some_and(|t| Instant::now() < t) {
            return Some(stopped_error());
        }
        let label = match op {
            Op::Execute { call } => self
                .engine
                .registry()
                .get(&call.command)
                .map(|c| c.meta().label_vi.to_string())
                .unwrap_or_else(|| call.command.clone()),
            Op::Batch { calls, .. } => format!("{} lệnh một lượt", calls.len()),
            _ => String::new(),
        };
        let e = &mut self.shell.errands;
        if let Some(u) = e.under.as_mut()
            && matches!(u.stage, Stage::Sent | Stage::Working)
        {
            u.stage = Stage::Working;
            u.steps = u.steps.saturating_add(1);
            u.last = label.clone();
            u.heard = Instant::now();
            self.shell.status_msg = format!("AI: {label}");
        }
        None
    }

    /// Word between the window and an agent or the relay (not a command).
    pub(crate) fn answer_errand_word(&mut self, request: Request) {
        let now = Instant::now();
        let e = &mut self.shell.errands;
        let mine = |u: &Option<Under>, id: u64| u.as_ref().is_some_and(|u| u.id == id);
        let answer: Result<Value, CmdError> = match &request.op {
            Op::NextErrand { .. } => {
                e.relay_seen = Some(now);
                let next = e.waiting.pop_front();
                if let (Some(errand), Some(u)) = (&next, e.under.as_mut())
                    && u.id == errand.id
                {
                    u.heard = now;
                }
                serde_json::to_value(next).map_err(|err| CmdError::internal(err.to_string()))
            }
            Op::ErrandSent {
                errand,
                conversation,
            } => {
                if mine(&e.under, *errand)
                    && let Some(u) = e.under.as_mut().filter(|u| u.stage == Stage::Waiting)
                {
                    u.stage = Stage::Sent;
                    u.heard = now;
                }
                crate::diag::note(
                    "errand",
                    &format!("{errand} in conversation {conversation}"),
                );
                Ok(Value::Null)
            }
            Op::ErrandFailed { errand, message } => {
                if let Some(u) = e
                    .under
                    .as_mut()
                    .filter(|u| u.id == *errand && u.stage.running())
                {
                    u.stage = Stage::Failed(message.clone());
                }
                crate::diag::note("errand", &format!("{errand} not handed on: {message}"));
                Ok(Value::Null)
            }
            Op::TaskBegin { request: asked } => {
                if let Some(u) = e
                    .under
                    .as_mut()
                    .filter(|u| matches!(u.stage, Stage::Sent | Stage::Working))
                {
                    u.stage = Stage::Working;
                    u.heard = now;
                }
                crate::diag::note("errand", &format!("the agent began: {asked}"));
                Ok(Value::Null)
            }
            Op::TaskEnd { status, .. } => {
                if let Some(u) = e
                    .under
                    .as_mut()
                    .filter(|u| matches!(u.stage, Stage::Sent | Stage::Working))
                {
                    u.stage = if status == "done" {
                        Stage::Done
                    } else {
                        Stage::Failed("AI báo không làm xong việc này.".to_string())
                    };
                    u.heard = now;
                }
                crate::diag::note("errand", &format!("the agent ended: {status}"));
                Ok(Value::Null)
            }
            Op::Grants => {
                serde_json::to_value(&e.granted).map_err(|err| CmdError::internal(err.to_string()))
            }
            _ => Err(CmdError::new(ErrorCode::UnknownCommand, "not word")),
        };
        request.answer(answer);
    }

    /// Every frame: an errand nobody takes starts Antigravity, then is given
    /// up; an agent silent too long is given up; "Dừng" wears off.
    pub(crate) fn poll_errands(&mut self) {
        if self.shell.errands.open {
            self.read_setup(false);
        }
        let now = Instant::now();
        let e = &mut self.shell.errands;
        if e.stopped_until.is_some_and(|t| now >= t) {
            e.stopped_until = None;
        }
        let relay_heard = e
            .relay_seen
            .is_some_and(|t| now.duration_since(t) < RELAY_WAIT);
        let Some(u) = e.under.as_mut() else {
            return;
        };
        match u.stage {
            Stage::Waiting => {
                let waited = now.duration_since(u.since);
                if waited >= TAKE_WAIT {
                    e.waiting.clear();
                    u.stage = Stage::Failed(
                        "Antigravity chưa nhận việc. Mở Antigravity, bấm «Nối với Antigravity» \
                         rồi gửi lại."
                            .to_string(),
                    );
                } else if waited >= RELAY_WAIT && !relay_heard && !e.launched {
                    e.launched = true;
                    crate::antigravity::launch();
                }
            }
            Stage::Sent | Stage::Working if now.duration_since(u.heard) >= QUIET => {
                u.stage = Stage::Failed("Đã lâu không thấy AI làm gì thêm.".to_string());
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iai_automation::Host;
    use iai_automation::bridge::{BridgeHost, Control};
    use iai_engine::CommandCall;
    use serde_json::json;

    #[test]
    fn the_ask_names_the_job_the_folders_and_the_rules() {
        let folder = Path::new(r"C:\Users\A\Downloads\Khach Lan");
        let form = ErrandForm {
            job: ErrandJob::IdPhotos,
            note: "chị Lan 8 tấm".to_string(),
            backdrop: "blue".to_string(),
            ..ErrandForm::default()
        };
        let images = Path::new(r"C:\Users\A\.gemini\antigravity\brain");
        let (title, prompt) = ask(&form, folder, Some(images), None);
        assert_eq!(title, "iAi · Ảnh thẻ cho nhiều người · Khach Lan");
        // Online AI is off unless asked for: iAi's own tools only.
        assert!(prompt.contains("Không dùng trợ lý vẽ ảnh"), "{prompt}");
        assert!(!prompt.contains("invoke_subagent"));
        for said in [
            "Làm ảnh thẻ 3x4 nền xanh cho từng người, mỗi người 4 tấm",
            "giấy ảnh 10x15",
            r"Thư mục ảnh của khách: C:\Users\A\Downloads\Khach Lan",
            r"Thư mục kết quả: C:\Users\A\Downloads\Khach Lan\iAi_ket_qua",
            "Người dùng dặn thêm: chị Lan 8 tấm",
            "id_photo.batch",
            "iai_task_begin",
            "Không dùng terminal",
            "iai_task_end",
        ] {
            assert!(prompt.contains(said), "{said}\n{prompt}");
        }
        let other = ErrandForm {
            job: ErrandJob::Other,
            note: "  Ghép 4 ảnh thành 1 tờ A4  ".to_string(),
            ..ErrandForm::default()
        };
        let (title, prompt) = ask(&other, folder, None, None);
        assert_eq!(title, "iAi · Việc khác · Khach Lan");
        assert!(prompt.contains("Việc: Ghép 4 ảnh thành 1 tờ A4\n"));
        assert!(!prompt.contains("Cách làm gợi ý") && !prompt.contains("dặn thêm"));
        let cards = ask(&ErrandForm::default(), folder, None, None).1;
        assert!(cards.contains("print.card_sheet") && cards.contains("can_cuoc.pdf"));
    }

    #[test]
    fn the_open_photo_goes_to_the_ai_and_comes_back_a_layer_of_its_tab() {
        assert_eq!(aspect(3000, 4000), "3:4");
        assert_eq!(aspect(1920, 1080), "16:9");
        assert_eq!(aspect(1000, 1010), "1:1");
        assert_eq!(aspect(4000, 2667), "3:2");
        assert_eq!(aspect(0, 0), "1:1");

        let folder = Path::new(r"C:\khach");
        let images = Path::new(r"C:\Users\A\.gemini\antigravity\brain");
        let photo = Photo {
            doc: 7,
            title: "chi_lan.jpg".to_string(),
            sent: PathBuf::from(r"C:\khach\iAi_ket_qua\chi_lan_gui_AI.jpg"),
            keep: PathBuf::from(r"C:\khach\iAi_ket_qua\chi_lan_AI.jpg"),
            aspect: "3:4",
        };
        let form = ErrandForm {
            job: ErrandJob::EditPhoto,
            note: "thay áo sơ mi trắng".to_string(),
            ..ErrandForm::default()
        };
        let (title, prompt) = ask(&form, folder, Some(images), Some(&photo));
        assert_eq!(title, "iAi · Sửa ảnh · chi_lan.jpg");
        for said in [
            "theo lời dặn: thay áo sơ mi trắng",
            r"Ảnh gửi AI: C:\khach\iAi_ket_qua\chi_lan_gui_AI.jpg",
            "Tab gốc trong iAi: doc 7",
            "invoke_subagent với TypeName \"image-generator\"",
            "khung 3:4",
            "iai_layer_place với {\"doc\": 7, \"from\": {\"path\":",
            "\"fit\": \"cover\"",
            r"C:\khach\iAi_ket_qua\chi_lan_AI.jpg",
            "mặt nạ (mask)",
            "Không được nói \"giữ nguyên 100%\"",
        ] {
            assert!(prompt.contains(said), "{said}\n{prompt}");
        }
        assert!(!prompt.contains("Không dùng trợ lý vẽ ảnh") && !prompt.contains("dặn thêm"));

        // What is sent: the tab flattened, as a JPEG in the results' folder.
        let mut app = App::new();
        app.shell.ui.show_welcome = false;
        let results = std::env::temp_dir().join(format!("iai-photo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&results);
        std::fs::create_dir_all(&results).unwrap();
        let photo = app.photo_to_send(&results, ErrandJob::EditPhoto).unwrap();
        let sent = image::open(&photo.sent).unwrap();
        let canvas = &app.docs.active().canvas;
        assert!(sent.width() <= 2048 && sent.height() <= 2048);
        assert_eq!(photo.aspect, aspect(canvas.width, canvas.height));
        assert_eq!(photo.doc, app.docs.active().id.0);
        assert!(photo.keep.starts_with(&results) && !photo.keep.exists());
        // A second send keeps the first copy.
        let again = app.photo_to_send(&results, ErrandJob::EditPhoto).unwrap();
        assert_ne!(again.sent, photo.sent);
        let _ = std::fs::remove_dir_all(&results);
    }

    #[test]
    fn an_id_photo_is_drawn_on_with_its_gap_grey_and_laid_back_face_on_face() {
        let folder = Path::new(r"C:\khach");
        let images = Path::new(r"C:\Users\A\.gemini\antigravity\brain");
        let photo = Photo {
            doc: 4,
            title: "chi_lan.jpg".to_string(),
            sent: PathBuf::from(r"C:\khach\iAi_ket_qua\chi_lan_gui_AI.jpg"),
            keep: PathBuf::from(r"C:\khach\iAi_ket_qua\chi_lan_AI.jpg"),
            aspect: "3:4",
        };
        // The clothes kept: only what the customer's photo missed is filled.
        let form = ErrandForm {
            job: ErrandJob::DressIdPhoto,
            ..ErrandForm::default()
        };
        let (title, prompt) = ask(&form, folder, Some(images), Some(&photo));
        assert_eq!(title, "iAi · Thay áo ảnh thẻ · chi_lan.jpg");
        for said in [
            "giữ nguyên áo khách đang mặc",
            "vùng XÁM",
            "Tab ảnh thẻ trong iAi: doc 4",
            "invoke_subagent với TypeName \"image-generator\"",
            "vào vùng màu xám",
            "GIỮ NGUYÊN khuôn mặt",
            "Khung 3:4",
            "id_photo.dress {\"doc\": 4,",
            "\"keepClothes\": true",
            "KHÔNG dùng iai_layer_place",
            r"C:\khach\iAi_ket_qua\chi_lan_AI.jpg",
            "Không được nói \"giữ nguyên 100%\"",
        ] {
            assert!(prompt.contains(said), "{said}\n{prompt}");
        }
        assert!(!prompt.contains("Không dùng trợ lý vẽ ảnh"));
        // Other clothes: the AI dresses the person and fills the gap too.
        let form = ErrandForm {
            job: ErrandJob::DressIdPhoto,
            shirt: "vest".to_string(),
            note: "cà vạt xanh".to_string(),
            ..ErrandForm::default()
        };
        let (_, prompt) = ask(&form, folder, Some(images), Some(&photo));
        for said in [
            "mặc áo vest đen, bên trong sơ mi trắng",
            "\"keepClothes\": false",
            "Người dùng dặn thêm: cà vạt xanh",
        ] {
            assert!(prompt.contains(said), "{said}\n{prompt}");
        }
    }

    #[test]
    fn online_ai_draws_only_hard_cases_it_was_allowed_and_never_id_cards() {
        let folder = Path::new(r"C:\khach");
        let images = Path::new(r"C:\Users\A\.gemini\antigravity\brain");
        let dressed = ErrandForm {
            job: ErrandJob::IdPhotos,
            online: true,
            shirt: "white_shirt".to_string(),
            ..ErrandForm::default()
        };
        let prompt = ask(&dressed, folder, Some(images), None).1;
        for said in [
            "Mọi người mặc áo sơ mi trắng",
            "invoke_subagent với TypeName \"image-generator\"",
            r"Ảnh vẽ xong nằm trong thư mục C:\Users\A\.gemini\antigravity\brain",
            "ẢNH ĐÃ THAY ÁO",
            "không được nói \"giữ nguyên 100%\"",
        ] {
            assert!(prompt.contains(said), "{said}\n{prompt}");
        }
        assert!(!prompt.contains("Không dùng trợ lý vẽ ảnh"));
        // Online, clothes kept: hard cases only, the usual steps.
        let kept = ErrandForm {
            shirt: String::new(),
            ..dressed.clone()
        };
        let prompt = ask(&kept, folder, Some(images), None).1;
        assert!(prompt.contains("image-generator") && !prompt.contains("ẢNH ĐÃ THAY ÁO"));
        // ID cards are never drawn anew, whatever the switch says.
        let cards = ErrandForm {
            job: ErrandJob::IdCards,
            online: true,
            ..ErrandForm::default()
        };
        let prompt = ask(&cards, folder, Some(images), None).1;
        assert!(prompt.contains("Không dùng trợ lý vẽ ảnh") && !prompt.contains("invoke_subagent"));
        // No folder for the pictures (no Antigravity): offline, through and through.
        let prompt = ask(&dressed, folder, None, None).1;
        assert!(prompt.contains("Không dùng trợ lý vẽ ảnh") && !prompt.contains("sơ mi"));
    }

    /// The window's frames, as `poll_agent_link` answers what comes in.
    fn frames_until(app: &mut App, done: &std::thread::JoinHandle<impl Send>) {
        let start = Instant::now();
        while !done.is_finished() {
            assert!(start.elapsed() < Duration::from_secs(60));
            app.poll_agent_link();
            app.poll_errands();
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn an_errand_goes_to_the_relay_and_the_agents_work_is_followed_until_stopped() {
        let mut app = App::new();
        app.shell.ui.show_welcome = false;
        let folder = std::env::temp_dir().join(format!("iai-errand-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        app.shell.errands.folder = Some(folder.clone());
        let file = std::env::temp_dir().join(format!("iai-errand-{}.json", std::process::id()));
        app.shell.agent_link = Some(app.start_agent_link(file.clone()).unwrap());
        let control = Control::read(&file).unwrap();

        app.send_errand(&ErrandForm {
            job: ErrandJob::ScanCleanup,
            ..ErrandForm::default()
        });
        assert!(app.errand_view().run.unwrap().running);
        assert_eq!(
            app.errand_view().run.unwrap().stage,
            "Đang chuyển việc sang Antigravity…"
        );
        // No second errand while one is under way.
        app.send_errand(&ErrandForm::default());
        assert_eq!(app.shell.errands.waiting.len(), 1);
        assert!(app.shell.errands.waiting[0].title.contains("Làm sạch"));

        // The relay takes it and hands it on.
        let relay = std::thread::spawn({
            let control = control.clone();
            move || {
                let mut relay = BridgeHost::connect(&control).unwrap();
                let errand = relay.next_errand("antigravity").unwrap().unwrap();
                let again = relay.next_errand("antigravity").unwrap();
                relay.errand_sent(errand.id, "c0ffee").unwrap();
                (errand, again)
            }
        });
        frames_until(&mut app, &relay);
        let (errand, again) = relay.join().unwrap();
        assert!(errand.title.contains("Làm sạch ảnh chụp giấy tờ") && again.is_none());
        assert!(errand.prompt.contains("image.scan_cleanup"));
        assert_eq!(
            app.errand_view().run.unwrap().stage,
            "Gemini đã nhận việc, đang đọc yêu cầu…"
        );

        // The agent works on the window: each command is a step.
        let agent = std::thread::spawn({
            let control = control.clone();
            move || {
                let mut agent = BridgeHost::connect(&control).unwrap();
                agent.task_begin("Làm sạch ảnh chụp giấy tờ");
                let grants = agent.grants();
                agent
                    .execute(CommandCall::new(
                        "file.new",
                        json!({"widthPx": 20, "heightPx": 10}),
                    ))
                    .unwrap();
                grants
            }
        });
        frames_until(&mut app, &agent);
        let grants = agent.join().unwrap();
        assert_eq!(grants.read, [folder.clone()]);
        assert_eq!(grants.write, [folder.join(RESULTS)]);
        let run = app.errand_view().run.unwrap();
        assert_eq!(
            (run.steps, run.stage.as_str()),
            (1, "Gemini đang làm trên iAi")
        );
        assert!(run.running && run.results.is_some());

        // "Dừng": the agent's next calls are refused.
        app.stop_errand();
        let stopped = std::thread::spawn(move || {
            let mut agent = BridgeHost::connect(&control).unwrap();
            let change = agent.execute(CommandCall::new(
                "file.new",
                json!({"widthPx": 4, "heightPx": 4}),
            ));
            let look = agent.execute(CommandCall::new("doc.list", json!({})));
            agent.task_end("cancelled", "");
            (change, look)
        });
        frames_until(&mut app, &stopped);
        let (change, look) = stopped.join().unwrap();
        assert_eq!(change.unwrap_err().code, ErrorCode::PermissionDenied);
        assert_eq!(look.unwrap_err().code, ErrorCode::PermissionDenied);
        let run = app.errand_view().run.unwrap();
        assert_eq!(run.stage, "Đã dừng.");
        assert!(!run.running);
        app.set_agent_control(false);
        let _ = std::fs::remove_dir_all(&folder);
    }
}

/// Opt-in, against the real Antigravity: IAI_ERRAND_PROBE is a folder of
/// photos. A window with no screen lets AI in through the real control
/// file, sends an errand on that folder ("Làm sạch ảnh chụp giấy tờ"
/// unless told otherwise) and follows it for up to ten minutes; what it
/// heard is printed.
#[cfg(test)]
mod probe {
    use super::*;

    #[test]
    #[ignore]
    fn probe_errand_end_to_end() {
        let Ok(folder) = std::env::var("IAI_ERRAND_PROBE") else {
            return;
        };
        // A window the person has open lets AI in: its file is not to be
        // taken, nor its agents' calls drawn into a trial.
        if crate::agent::control_file().exists() {
            println!("another iAi window lets AI in: not run");
            return;
        }
        let mut app = App::new();
        app.shell.ui.show_welcome = false;
        app.shell.errands.folder = Some(PathBuf::from(&folder));
        app.set_agent_control(true);
        assert!(app.agent_control_on(), "{:?}", app.shell.agent_link_error);
        // IAI_ERRAND_FORM: a JSON file of the window's fields, {"job": "ids" |
        // "photo" | "dress" | "other" | "scan" (the default), "note", "online",
        // "shirt"}.
        let given: serde_json::Value = std::env::var("IAI_ERRAND_FORM")
            .ok()
            .and_then(|f| std::fs::read(f).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let text = |k: &str| given[k].as_str().unwrap_or_default().to_string();
        let form = ErrandForm {
            job: match text("job").as_str() {
                "ids" => ErrandJob::IdPhotos,
                "photo" => ErrandJob::EditPhoto,
                "dress" => ErrandJob::DressIdPhoto,
                "other" => ErrandJob::Other,
                _ => ErrandJob::ScanCleanup,
            },
            note: text("note"),
            online: given["online"].as_bool().unwrap_or(false),
            shirt: text("shirt"),
            ..ErrandForm::default()
        };
        println!("form: {form:?}");
        let go = |app: &mut App, command: &str, args: serde_json::Value| {
            let reply = app
                .execute_command(iai_engine::CommandCall::new(command, args))
                .unwrap();
            let waited = Instant::now();
            while let iai_engine::Reply::Started { job } = &reply {
                app.poll_engine();
                if !matches!(
                    app.engine.job_state(*job),
                    Some(iai_engine::JobState::Running { .. })
                ) || waited.elapsed() > Duration::from_secs(120)
                {
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        // "open": a photo opened in the window first, for the jobs on it; an
        // ID photo made of it to dress.
        let open = text("open");
        if !open.is_empty() {
            go(&mut app, "file.open", serde_json::json!({"path": open}));
            if form.job == ErrandJob::DressIdPhoto {
                go(
                    &mut app,
                    "id_photo.make",
                    serde_json::json!({"size": "3x4", "backdrop": "white"}),
                );
            }
            let doc = app.docs.active();
            println!(
                "opened: {} {}x{}, {} layer(s)",
                doc.tab_title(),
                doc.canvas.width,
                doc.canvas.height,
                doc.canvas.layer_count()
            );
        }
        app.send_errand(&form);
        println!("granted: {:?}", app.shell.errands.granted);
        let start = Instant::now();
        let mut said = String::new();
        loop {
            app.poll_agent_link();
            app.poll_errands();
            let run = app.errand_view().run.unwrap();
            let now = format!("{} · {} bước · {}", run.stage, run.steps, run.last);
            if now != said {
                println!("{:>6.1}s  {now}", start.elapsed().as_secs_f32());
                said = now;
            }
            if !run.running || start.elapsed() > Duration::from_secs(600) {
                println!("docs open: {}", app.docs.documents.len());
                let ids: Vec<u32> = app.docs.documents.iter().map(|d| d.id.0).collect();
                for id in ids {
                    let seen = app.execute_command(iai_engine::CommandCall::new(
                        "doc.inspect",
                        serde_json::json!({"doc": id}),
                    ));
                    if let Ok(reply) = seen {
                        let r = reply.result().cloned().unwrap_or_default();
                        let names: Vec<&str> = r["layers"]
                            .as_array()
                            .map(|a| a.iter().filter_map(|l| l["name"].as_str()).collect())
                            .unwrap_or_default();
                        println!("doc {id} «{}»: layers {names:?}", r["title"]);
                    }
                    // How each tab looks, in the trial's results' folder.
                    let shot = Path::new(&folder)
                        .join(RESULTS)
                        .join(format!("_probe_doc{id}.jpg"));
                    let _ = app.execute_command(iai_engine::CommandCall::new(
                        "file.export",
                        serde_json::json!({"doc": id, "path": shot.display().to_string()}),
                    ));
                }
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        app.set_agent_control(false);
    }
}

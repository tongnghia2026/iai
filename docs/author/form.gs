/**
 * iAi - biểu mẫu nhận tin từ người dùng app.
 *
 * App gửi tới đây ba loại tin: "Góp ý, báo lỗi", "Cần tính năng riêng" và
 * thông tin của đơn vị tài trợ (tên, liên hệ, logo). Mỗi tin thành một email
 * tới TO, một dòng trong bảng tính, và file đính kèm nằm trong một thư mục
 * Google Drive.
 *
 * Cách bật: xem HUONG_DAN_BAT_BIEU_MAU_GOOGLE.md nằm cạnh file này.
 * Phần app: src/author_mail.rs (gửi), src/author_news.rs (đọc địa chỉ biểu mẫu).
 */

var TO = 'minhnghia718@gmail.com';
var FOLDER_NAME = 'iAi - file người dùng gửi';
// Quá số tin này trong một ngày thì từ chối: địa chỉ biểu mẫu là công khai.
var MAX_PER_DAY = 80;
var MAX_BODY_CHARS = 16 * 1024 * 1024;
var MAX_FILES = 3;
var KINDS = {
  sponsor: 'Tài trợ dưới tên công ty, tổ chức',
  feedback: 'Góp ý, báo lỗi',
  feature: 'Cần tính năng riêng'
};
var COLUMNS = ['Lúc', 'Loại', 'Tên', 'Liên hệ', 'Nội dung', 'Bản app', 'File đính kèm'];

/** Chạy một lần sau khi dán mã: xin quyền, kẻ tiêu đề cột, gửi một email thử. */
function setup() {
  var sheet = SpreadsheetApp.getActiveSpreadsheet().getSheets()[0];
  if (sheet.getLastRow() === 0) {
    sheet.appendRow(COLUMNS);
    sheet.setFrozenRows(1);
  }
  getFolder();
  MailApp.sendEmail({
    to: TO,
    subject: '[iAi] Biểu mẫu đã sẵn sàng',
    body: 'Đây là email thử. Từ giờ tin người dùng gửi từ app sẽ về hộp thư này.'
  });
}

/** Mở địa chỉ biểu mẫu bằng trình duyệt: chỉ để biết nó đang chạy. */
function doGet() {
  return ContentService.createTextOutput('iAi: biểu mẫu đang chạy.');
}

/** App gửi tin tới đây. Trả lời {"ok": true} khi tin đã tới tác giả. */
function doPost(e) {
  try {
    var body = (e && e.postData && e.postData.contents) || '';
    if (!body || body.length > MAX_BODY_CHARS) {
      return reply(false, 'size');
    }
    var msg = JSON.parse(body);
    if (msg.app !== 'iAi' || !KINDS.hasOwnProperty(msg.kind)) {
      return reply(false, 'not for this form');
    }
    if (!underDailyLimit()) {
      return reply(false, 'busy');
    }

    var label = KINDS[msg.kind];
    var name = clip(msg.name, 200);
    var contact = clip(msg.contact, 300);
    var text = clip(msg.text, 6000);
    var build = clip(msg.build, 100);

    var blobs = (msg.files || []).slice(0, MAX_FILES).map(function (file) {
      return Utilities.newBlob(
        Utilities.base64Decode(String(file.data || '')),
        safeType(file.type),
        safeName(file.name)
      );
    });
    var links = [];
    if (blobs.length) {
      var folder = getFolder();
      var stamp = Utilities.formatDate(new Date(), Session.getScriptTimeZone(), 'yyyyMMdd-HHmmss');
      blobs.forEach(function (blob) {
        links.push(folder.createFile(blob.copyBlob().setName(stamp + ' ' + blob.getName())).getUrl());
      });
    }

    SpreadsheetApp.getActiveSpreadsheet().getSheets()[0].appendRow(
      [new Date(), label, cell(name), cell(contact), cell(text), cell(build), links.join('\n')]
    );

    MailApp.sendEmail({
      to: TO,
      subject: '[iAi] ' + label + (name ? ' - ' + name : ''),
      body: label + '\n\n' +
        'Tên: ' + (name || '(không ghi)') + '\n' +
        'Liên hệ: ' + (contact || '(không ghi)') + '\n' +
        'Bản app: ' + build + '\n\n' +
        text + '\n' +
        (links.length ? '\nFile đính kèm (cũng nằm trong Drive):\n' + links.join('\n') + '\n' : ''),
      attachments: blobs
    });
    return reply(true, '');
  } catch (err) {
    return reply(false, String(err));
  }
}

function reply(ok, why) {
  return ContentService
    .createTextOutput(JSON.stringify({ ok: ok, why: why }))
    .setMimeType(ContentService.MimeType.JSON);
}

/** Đếm số tin trong ngày; quá MAX_PER_DAY thì thôi nhận. */
function underDailyLimit() {
  var lock = LockService.getScriptLock();
  lock.waitLock(10000);
  try {
    var props = PropertiesService.getScriptProperties();
    var today = Utilities.formatDate(new Date(), Session.getScriptTimeZone(), 'yyyyMMdd');
    var seen = props.getProperty('day') === today ? Number(props.getProperty('count') || 0) : 0;
    if (seen >= MAX_PER_DAY) {
      return false;
    }
    props.setProperties({ day: today, count: String(seen + 1) });
    return true;
  } finally {
    lock.releaseLock();
  }
}

function getFolder() {
  var found = DriveApp.getFoldersByName(FOLDER_NAME);
  return found.hasNext() ? found.next() : DriveApp.createFolder(FOLDER_NAME);
}

function clip(value, max) {
  return String(value == null ? '' : value).slice(0, max);
}

/** Chữ bắt đầu bằng = + - @ sẽ bị bảng tính coi là công thức: chặn lại. */
function cell(text) {
  return /^[=+\-@]/.test(text) ? "'" + text : text;
}

function safeName(name) {
  var clean = String(name || 'file').replace(/[^A-Za-z0-9._-]/g, '_').slice(0, 60);
  return clean || 'file';
}

function safeType(type) {
  var allowed = ['image/png', 'image/jpeg', 'application/zip'];
  return allowed.indexOf(type) >= 0 ? type : 'application/octet-stream';
}

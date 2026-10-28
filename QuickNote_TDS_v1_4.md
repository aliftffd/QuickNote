# QuickNote — Technical Design Specification v1.4

Status: dasar implementasi hasil konsolidasi diskusi, belum merupakan hasil pengujian.

## 1. Tujuan dan batas jaminan

QuickNote adalah aplikasi catatan TUI dengan satu daemon lokal, beberapa jendela, SQLite, dan AI sebagai fitur inti untuk mengelompokkan serta menata seluruh catatan berdasarkan kategori. Satu jendela menjadi penulis aktif; jendela lain menjadi mirror. Mode manual adalah fallback ketika AI tidak tersedia, bukan pengganti fungsi utama pengelompokan otomatis.

- Target respons pada driver yang sudah memperoleh kepemilikan: p95 waktu event ketikan diterima klien hingga render TUI selesai <16 ms, termasuk saat AI berjalan. Waktu perpindahan kepemilikan diukur terpisah. Latensi sampai piksel tampil di monitor juga melibatkan terminal dan compositor.
- Respons editor tidak menunggu IPC, inferensi, atau commit database.
- Transaksi yang berhasil di-commit dengan WAL + FULL menjadi batas durabilitas, dengan asumsi filesystem dan perangkat penyimpanan memenuhi kontrak sinkronisasi.
- Retry buffer di RAM membantu menghadapi crash daemon selama klien masih hidup. Operasi yang belum mempunyai salinan durabel tetap dapat hilang jika seluruh pemegangnya berhenti.
- Berhasil mengirim ke soket, menerima operasi di daemon, menerapkan operasi di RAM, dan menyimpan operasi ke disk adalah keadaan yang berbeda.

Arsitektur ini memakai pengurutan operasi dan transaksi state. Tidak mensyaratkan event sourcing penuh atau riwayat permanen seluruh ketikan.

## 2. Komponen dan otoritas

### Klien TUI

Klien menangani input, rendering optimistis, cursor, overlay pencarian, indikator penyimpanan, dan retry buffer. Klien tidak menulis database utama. Status mirror menolak perubahan teks lokal.

### Actor state pada daemon

Satu actor mengurutkan perubahan teks, perpindahan kepemilikan, revisi, dan pengambilan snapshot. RAM actor menjadi otoritas state aktif selama daemon berjalan. Snapshot selalu memuat teks dan revisi yang diambil secara konsisten.

### Worker database

Worker khusus memiliki koneksi penulis SQLite. Actor mengirim checkpoint immutable kepadanya melalui antrean terbatas; pekerjaan disk tidak berjalan di jalur rendering maupun menahan pengurutan state selama I/O. Checkpoint diproses berurutan.

### Worker AI — pengelompokan seluruh catatan

Baseline memakai satu pekerjaan inferensi melalui `spawn_blocking`. Permintaan digabung per `note_id`: hanya revisi terbaru untuk catatan yang sama yang dipertahankan, sementara pekerjaan untuk catatan lain tetap tercatat. Antrean siap di RAM dibatasi; metadata pekerjaan tertunda dipersistensikan bersama state yang relevan agar seluruh catatan akhirnya dapat diproses. Worker tidak memegang lock state selama inferensi dan tidak langsung mengubah state catatan.

## 3. Identitas, operasi, dan revisi

Setiap operasi perubahan memiliki `note_id`, `stream_id`, `seq`, `base_revision`, dan token kepemilikan. `stream_id` tetap digunakan untuk retry operasi yang sama; `seq` meningkat dan tidak digunakan kembali untuk isi operasi yang berbeda.

- `applied_revision`: revisi teks yang sudah diterapkan pada state aktif actor.
- `committed_revision`: revisi teks dalam checkpoint yang sudah berhasil di-commit.
- `last_applied_seq`: urutan kontigu yang sudah diterapkan pada RAM untuk suatu stream.
- `last_committed_seq`: urutan kontigu yang efeknya sudah tercakup dalam commit untuk stream tersebut.
- `daemon_instance_id`: identitas baru pada setiap restart, agar revisi dan epoch dari instance berbeda tidak tertukar.

Klien mengirim operasi edit yang terdefinisi, misalnya insert/delete, bukan tombol mentah. Posisi edit menggunakan offset byte UTF-8 pada revisi dasarnya; daemon memeriksa batas karakter dan validitas rentang. Navigasi cursor tidak harus menaikkan revisi konten.

Daemon memeriksa identitas operasi, urutan tanpa celah, revisi dasar, dan kepemilikan. Duplikat yang sudah diterapkan tidak diterapkan ulang. Setelah restart, hanya metadata yang sudah di-commit yang dapat membuktikan suatu operasi pernah tersimpan. Operasi di atas penanda tersebut tetap membutuhkan replay atau recovery.

Penolakan harus menghasilkan respons eksplisit. Jika penolakan menghentikan urutan suatu stream, klien menyelesaikan rollback/recovery dan melakukan sinkronisasi sebelum memulai stream baru; jangan melewati nomor urut lalu menganggap semua operasi sebelumnya tersimpan.

## 4. Kepemilikan dan perpindahan fokus

Fokus Hyprland menjadi pemicu permintaan kepemilikan. Daemon adalah satu-satunya penerbit token `(daemon_instance_id, owner_epoch)` dan pemberi izin menjadi driver. Klaim yang bersamaan diurutkan oleh actor.

Alur normal:

1. B mengirim `ClaimSession`; perubahan teks lokal B ditahan hingga izin diberikan.
2. Daemon mengirim `Freeze` ke A. A berhenti menerima edit baru, mengirim seluruh operasi tertunda, lalu `FreezeAck(last_seq)`.
3. Daemon memastikan seluruh operasi hingga `last_seq` sudah diterapkan pada RAM. Ini belum berarti semuanya sudah di-commit.
4. Daemon mencabut token A, menaikkan epoch, lalu memberi B snapshot konsisten dan token baru.
5. B memasang snapshot sebelum mengaktifkan input dan rendering optimistis.

Timeout freeze ditargetkan 500 ms. Jika A tidak merespons, daemon mencabut kepemilikannya dan dapat mengaktifkan B berdasarkan state yang tersedia. Edit A yang belum diterapkan tidak disisipkan kemudian ke teks B secara otomatis.

Retry dari token kedaluwarsa diperiksa terhadap deduplikasi. Operasi yang terbukti sudah tersimpan mendapat hasil yang sesuai; operasi yang belum tersimpan masuk jalur recovery. Fork recovery harus membawa snapshot lokal/base yang diperlukan untuk memahami edit, bukan delta tanpa konteks. Kegagalan menulis fork tidak boleh dianggap berhasil diamankan.

## 5. Autosave, transaksi, dan DurableAck

SQLite menggunakan `journal_mode=WAL` dan `synchronous=FULL` pada koneksi penulis. Worker memeriksa keberhasilan konfigurasi dan hasil transaksi.

Autosave dijadwalkan ketika editor idle selama 1 detik atau target interval 5 detik sejak checkpoint sukses terakhir tercapai selama state dirty. Jika state mulai dirty setelah lama idle, jadwal penyimpanan tidak boleh menunggu siklus yang sudah lewat. Timer ini menjadwalkan pekerjaan; antrean dan latensi disk tetap dapat menunda penyelesaian.

Satu checkpoint menyimpan secara atomik teks, revisi checkpoint, serta penanda deduplikasi per stream yang benar-benar tercakup. Metadata sesi terkait disimpan secara konsisten. Indeks FTS diperbarui bersama transaksi konten terkait.

Worker membalas keberhasilan untuk checkpoint yang ditulis. Daemon mengirim `DurableAck` berdasarkan checkpoint tersebut, bukan berdasarkan revisi RAM yang mungkin sudah lebih baru. ACK mengidentifikasi instance, stream, catatan, urutan yang tercakup, dan revisi commit terkait.

Klien mempertahankan operasi di retry buffer hingga operasi itu tercakup DurableAck. Hilangnya ACK berarti status belum diketahui: transaksi mungkin sudah tersimpan. ACK duplikat tidak boleh menghapus operasi lokal yang lebih baru.

## 6. Reconnect dan restart daemon

Saat soket putus, klien mempertahankan retry buffer dan menampilkan status belum tersimpan. Saat reconnect, klien dan daemon bertukar identitas instance/stream, state commit, serta snapshot yang relevan.

Setelah restart, daemon memuat state persisten dari SQLite; token kepemilikan lama tidak lagi berlaku. Klien harus melakukan sinkronisasi ulang sebelum memakai delta atau snapshot instance sebelumnya.

Replay ke catatan utama hanya diizinkan setelah kepemilikan baru diberikan dan basis operasi cocok dengan state yang akan dilanjutkan. Contoh: database menyimpan hingga seq 40; klien masih mempunyai 41–45 yang berurutan dan cocok dengan basis tersebut. Jika syaratnya terpenuhi, operasi itu diterapkan dan di-commit kembali.

Jika ada perubahan lain yang membuat basis tidak cocok, operasi tidak dipaksakan ke catatan utama. Buat recovery/fork dengan identitas yang stabil agar pengulangan recovery tidak menghasilkan duplikat tanpa batas.

## 7. Penutupan dengan Esc dan recovery darurat

Esc memulai prosedur penutupan:

1. Klien menghentikan edit baru dan mengirim seluruh operasi tertunda.
2. Klien mengirim `Flush(note_id, stream_id, last_seq)` sebagai barrier terhadap operasi tersebut.
3. Daemon menjadwalkan commit yang mencakup barrier, lalu membalas DurableAck setelah commit sukses.
4. Klien keluar hanya setelah seluruh perubahan lokal tercakup ACK atau berhasil diamankan melalui recovery durabel.

Timeout 250 ms adalah pemicu fallback recovery, bukan jaminan waktu penyelesaian disk maupun waktu keluar. Penulisan recovery juga dapat memerlukan waktu lebih lama.

Recovery menyimpan snapshot lokal, identitas catatan dan stream, basis revisi, serta operasi yang belum dikonfirmasi. Lokasi default adalah `$XDG_DATA_HOME/quicknote/recovery/`, dengan fallback `~/.local/share/quicknote/recovery/`. Direktori memakai izin 0700 dan berkas 0600.

Urutan penulisan: berkas sementara pada direktori yang sama → tulis lengkap → `fsync(file)` → rename ke nama final unik → `fsync(directory)`. Kesalahan write, sync, atau rename menghasilkan peringatan dan mempertahankan buffer; aplikasi tidak keluar otomatis.

Saat startup, recovery diperiksa dan dicocokkan dengan metadata commit. Recovery tidak otomatis menimpa catatan yang lebih baru. Berkas yang telah tersinkronisasi tetapi belum masuk catatan utama ditampilkan sebagai `Recovered locally`, bukan `Saved` pada database utama.

## 8. Mirror dan sinkronisasi snapshot

Broadcast membawa perubahan beserta identitas catatan, instance daemon, dan revisi berurutan. Antrean kirim setiap koneksi dibatasi; penulisan ke mirror lambat tidak memblokir actor.

Saat penerima mengalami `Lagged` atau menemukan celah revisi:

1. Daemon memasang receiver untuk koneksi tersebut sebelum menangkap snapshot.
2. Snapshot diambil dari RAM actor, dengan teks dan `applied_revision = r` yang konsisten.
3. Perubahan yang datang selama transfer ditampung dalam buffer terbatas.
4. Klien memasang snapshot, membuang perubahan dengan revisi ≤ r, kemudian menerapkan r+1, r+2, dan seterusnya.

`seq` per stream tidak dipakai sebagai pengganti revisi catatan. Jika buffer kembali tertinggal atau urutan tidak lengkap, ulangi sinkronisasi. Koneksi yang terus tidak mampu mengikuti dapat diputus tanpa menghilangkan perubahan milik driver.

## 9. IPC dan batas ukuran

- Unix domain socket berada di `%t/quicknote.sock` pada unit systemd user; klien menggunakan `$XDG_RUNTIME_DIR/quicknote.sock`.
- `quicknote.socket` memakai `SocketMode=0600`. Daemon menerima listening file descriptor dari systemd, bukan melakukan bind ulang pada socket yang sama.
- Format baseline: JSON UTF-8 dengan header panjang 4 byte big-endian.
- Batas payload satu frame: 1 MiB = 1.048.576 byte, tidak termasuk header. Panjang diperiksa penerima sebelum alokasi payload; pengirim memeriksa hasil serialisasi.
- Batas konten satu catatan: 500 KiB = 512.000 byte UTF-8 mentah. Batas ini terpisah dari ukuran frame.
- Batch operasi dibagi menjadi frame berurutan tanpa mengubah identitas operasinya. Snapshot atau operasi tunggal yang membesar akibat escaping membutuhkan chunking terbatas; reassembly divalidasi sebelum state berubah.
- Jumlah koneksi, panjang antrean, jumlah transfer parsial, ukuran reassembly, dan waktu tunggu frame dibatasi. Pesan malformed atau pelanggaran kontrak menghasilkan error atau pemutusan koneksi sesuai jenis kegagalan.

Format biner dapat dievaluasi jika pengukuran membenarkannya; tidak diasumsikan memiliki rasio ukuran 1:1. Penggantian codec tetap mempertahankan versioning protokol dan batas payload.

## 10. AI dan integritas telemetri

AI adalah komponen inti pengelompokan otomatis seluruh QuickNote, mencakup catatan baru maupun catatan lama yang belum memiliki hasil relevan. Daftar catatan menggunakan kategori manual jika sudah ditetapkan pengguna, atau prediksi aktif yang masih relevan. Riwayat evaluasi tetap berada di `ai_events` dan tidak disamakan dengan kategori aktif untuk tampilan.

Inferensi dijadwalkan setelah idle 4 detik atau setelah perubahan teks yang cukup besar, misalnya ≥40 karakter edit sejak evaluasi terakhir. Permintaan berulang dikoalesensikan; perubahan tidak dinilai hanya dari selisih panjang akhir string.

- Maksimal satu inferensi berjalan. Untuk setiap note_id yang menunggu, pekerjaan mengacu pada revisi teks terbarunya; perubahan pada catatan A tidak boleh menghapus pekerjaan catatan B.
- Catatan aktif diprioritaskan dengan giliran berkala untuk backlog agar catatan lama tidak terus tertunda. Antrean siap di RAM tetap terbatas; pekerjaan lain tetap tercatat untuk penjadwalan berikutnya.
- Saat startup, pemulihan model, atau pergantian model_version, daemon memeriksa catatan yang belum dikategorikan atau hasil otomatisnya sudah basi. Metadata pekerjaan dan hasil yang dipersistensikan memungkinkan backlog dilanjutkan setelah restart. Kategori manual tetap dipertahankan.
- Jumlah thread internal ONNX dikonfigurasi eksplisit; baseline intra-op satu thread dan eksekusi sequential.
- Setiap permintaan membawa `daemon_instance_id`, `note_id`, `text_revision`, dan `model_version`.
- Worker memeriksa apakah permintaan masih relevan sebelum berjalan. Actor memeriksa lagi identitas dan revisinya setelah hasil selesai; hasil basi tidak memperbarui kategori aktif.
- Kategori yang ditetapkan pengguna memiliki prioritas terhadap prediksi otomatis.
- Model tidak tersedia atau error yang dapat ditangani mengaktifkan fallback: pengguna tetap dapat mencatat dan menetapkan kategori manual, sementara pengelompokan otomatis menampilkan status tertunda. Pekerjaan yang belum selesai dipertahankan dan dijadwalkan ulang ketika AI pulih. Baseline thread tidak mengisolasi crash native ONNX dari proses daemon; isolasi tersebut membutuhkan worker proses terpisah bila dijadikan persyaratan.

`ai_events` menyimpan event_id, note_id, snapshot teks immutable, text_revision, model_version, prediksi, skor model, waktu, status intervensi, dan kategori yang dikonfirmasi. Skor model tidak otomatis dianggap probabilitas yang terkalibrasi.

Status: `Pending`, `ExplicitConfirm`, `Corrected`, dan bila diperlukan `ImplicitAccept` untuk analitik perilaku. Intervensi selalu merujuk event_id/snapshot tertentu. Hanya ExplicitConfirm dan Corrected yang diekspor sebagai data berlabel; status lainnya dapat diekspor sebagai telemetri tanpa klaim ground truth. Ekspor tetap melalui `quicknote export --format jsonl --ai-events` dengan status tersedia untuk penyaringan.

## 11. Sesi, pencarian, dan indikator UI

Daemon menyimpan active_note_id dan cursor yang terkait dengan revisi tersimpan. Saat restart, cursor divalidasi terhadap panjang teks hasil pemulihan. Draft kosong dibersihkan pada startup maupun shutdown normal, hanya jika tidak memiliki perubahan tertunda atau recovery terkait.

Ctrl+F membuka overlay pencarian FTS5 dengan pemeringkatan bm25 tanpa menghilangkan state editor. Hasil FTS mencerminkan konten yang sudah masuk indeks; perubahan dirty pada catatan aktif tetap berasal dari RAM actor.

Daftar catatan dikelompokkan menurut kategori aktif. Catatan yang belum memiliki kategori ditempatkan pada kelompok `Belum dikategorikan`; pekerjaan otomatisnya tetap berada dalam backlog. Hasil sebelumnya yang sudah basi ditandai perlu diperbarui dan tidak dipresentasikan sebagai evaluasi atas revisi teks terbaru.

Indikator UI:

- `Unsaved`: terdapat perubahan lokal yang belum tercakup DurableAck.
- `Saving`: commit/flush sedang ditunggu.
- `Saved`: seluruh perubahan lokal pada catatan tersebut sudah tercakup DurableAck yang sesuai.
- `Recovered locally`: salinan recovery durabel tersedia, tetapi belum direkonsiliasi ke catatan utama.
- `Error`: operasi ditolak atau penyimpanan/recovery gagal; alasan ditampilkan dan buffer dipertahankan sampai terselesaikan.

Edit baru langsung membatalkan status Saved. Penolakan tidak boleh menghilangkan teks secara diam-diam atau menghasilkan indikator Saved yang keliru.

## 12. Kriteria verifikasi implementasi

1. Crash daemon sebelum/sesudah commit dan sebelum/sesudah pengiriman ACK: retry tidak menggandakan operasi dan metadata deduplikasi cocok dengan teks.
2. Perpindahan fokus dengan operasi A masih tertunda, timeout Freeze, dan koneksi yang pulih: operasi kedaluwarsa tidak menimpa B dan recovery dapat direkonsiliasi.
3. Autosave tertunda, subscribe bersamaan dengan edit, dan Lagged saat snapshot dikirim: mirror mengikuti revisi tanpa celah atau meminta resinkronisasi.
4. Disk penuh, izin write gagal, write parsial, dan sync gagal: tidak ada DurableAck palsu atau penutupan otomatis yang membuang buffer.
5. Paste besar, JSON escaping, chunk tidak lengkap, dan mirror lambat: batas memori/IPC tetap berlaku dan editor tidak menampilkan kepastian simpan yang keliru.
6. Inferensi lambat, hasil basi, dan model tidak tersedia: fallback pencatatan tetap berjalan; p95 rendering diukur pada terminal, mesin, dan beban yang dinyatakan.
7. Banyak catatan menunggu, perpindahan catatan aktif, restart, dan model yang pulih: pekerjaan hanya digabung untuk note_id yang sama, seluruh backlog mendapat giliran, dan kategori manual tidak ditimpa AI.

Process-kill menguji pemulihan proses, bukan bukti ketahanan mati listrik. Klaim durabilitas tetap dibatasi pada commit/sinkronisasi sukses serta kontrak filesystem dan perangkat penyimpanan.

## Referensi teknis

- SQLite synchronous dan WAL: https://www.sqlite.org/pragma.html#pragma_synchronous
- Linux fsync: https://man7.org/linux/man-pages/man2/fsync.2.html
- Linux send: https://man7.org/linux/man-pages/man2/send.2.html
- Tokio broadcast: https://docs.rs/tokio/latest/tokio/sync/broadcast/
- Tokio spawn_blocking: https://docs.rs/tokio/latest/tokio/task/fn.spawn_blocking.html
- ONNX Runtime threading: https://onnxruntime.ai/docs/performance/tune-performance/threading.html
- systemd socket activation: https://www.freedesktop.org/software/systemd/man/latest/systemd.socket.html

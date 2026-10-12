// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

// Host test of `receiver-delay.h`.
//
// What it proves: the calls this header makes land on the slots the engine's
// header declares, with the arguments and the return convention a C++
// compiler gives those declarations, and the probe refuses when an answer is
// wrong. `FakeReceiver` declares its virtuals in the order of
// `api/rtp_receiver_interface.h` at branch-heads/6422, on the vendored
// `RefCountInterface`, with `std::optional<double>` and `rtc::scoped_refptr`
// where the engine has them; the host compiler lays its vtable out, so the
// absolute-slot path is checked against a real vtable. The relative-slot
// path (Android arm64) is checked against a table built by hand.
//
// What it cannot prove: that the shipped binaries are laid out this way.
// That was read from the binaries themselves (see the header and ADR-0070,
// note of 2026-10-12) and is guarded at run time by the image identity pin
// in each platform shim.

#include <cstdint>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <optional>
#include <string>
#include <vector>

#include "api/crypto/frame_decryptor_interface.h"
#include "api/media_types.h"
#include "api/ref_count.h"
#include "api/scoped_refptr.h"
#include "receiver-delay.h"

namespace rd = lakko::receiver_delay;

namespace {

int g_failures = 0;
std::vector<std::string> g_log;

void Log(const char* line) { g_log.emplace_back(line); }

#define CHECK_THAT(cond)                                             \
  do {                                                               \
    if (!(cond)) {                                                   \
      std::fprintf(stderr, "FAIL %s:%d: %s\n", __FILE__, __LINE__,   \
                   #cond);                                           \
      ++g_failures;                                                  \
    }                                                                \
  } while (0)

class FakeDecryptor : public webrtc::FrameDecryptorInterface {
 public:
  void AddRef() const override { ++refs; }
  rtc::RefCountReleaseStatus Release() const override {
    --refs;
    return rtc::RefCountReleaseStatus::kOtherRefsRemained;
  }
  Result Decrypt(cricket::MediaType,
                 const std::vector<uint32_t>&,
                 rtc::ArrayView<const uint8_t>,
                 rtc::ArrayView<const uint8_t>,
                 rtc::ArrayView<uint8_t>) override {
    return Result(Status::kOk, 0);
  }
  size_t GetMaxPlaintextByteSize(cricket::MediaType, size_t size) override {
    return size;
  }
  mutable int refs = 1;
};

// The engine's `RtpReceiverInterface`, slot for slot. Types this test does
// not have are replaced by ones of no consequence to the slot order.
class FakeReceiver : public webrtc::RefCountInterface {
 public:
  // Slots 0 and 1; the base's virtual destructor is slots 2 and 3.
  void AddRef() const override { ++refs; }
  webrtc::RefCountReleaseStatus Release() const override {
    --refs;
    ++release_calls;
    return webrtc::RefCountReleaseStatus::kOtherRefsRemained;
  }

  virtual void* track() const { return Wrong(); }                    // 4
  virtual void* dtls_transport() const { return Wrong(); }           // 5
  virtual void* stream_ids() const { return Wrong(); }               // 6
  virtual void* streams() const { return Wrong(); }                  // 7
  virtual cricket::MediaType media_type() const {                    // 8
    ++media_type_calls;
    return kind;
  }
  virtual void* id() const { return Wrong(); }                       // 9
  virtual void* GetParameters() const { return Wrong(); }            // 10
  virtual bool SetParameters(const int&) {                           // 11
    Wrong();
    return false;
  }
  virtual void SetObserver(void*) { Wrong(); }                       // 12
  virtual void SetJitterBufferMinimumDelay(                          // 13
      std::optional<double> delay_seconds) {
    ++set_calls;
    last = delay_seconds;
  }
  virtual void* GetSources() const { return Wrong(); }               // 14
  virtual void SetFrameDecryptor(                                    // 15
      rtc::scoped_refptr<webrtc::FrameDecryptorInterface>) {
    Wrong();
  }
  virtual rtc::scoped_refptr<webrtc::FrameDecryptorInterface>        // 16
  GetFrameDecryptor() const {
    ++get_decryptor_calls;
    return rtc::scoped_refptr<webrtc::FrameDecryptorInterface>(decryptor);
  }
  virtual void SetDepacketizerToDecoderFrameTransformer(void*) {     // 17
    Wrong();
  }

  void* Wrong() const {
    ++wrong_calls;
    return nullptr;
  }

  ~FakeReceiver() override = default;

  cricket::MediaType kind = cricket::MEDIA_TYPE_VIDEO;
  webrtc::FrameDecryptorInterface* decryptor = nullptr;
  std::optional<double> last;
  int set_calls = 0;
  mutable int refs = 1;
  mutable int release_calls = 0;
  mutable int media_type_calls = 0;
  mutable int get_decryptor_calls = 0;
  mutable int wrong_calls = 0;
};

void test_layout_of_the_argument() {
  // The engine's argument is std::optional<double>; ours must be the same
  // bytes: the double first, the engaged flag at +8.
  CHECK_THAT(sizeof(std::optional<double>) == sizeof(rd::EngineOptionalDouble));
  std::optional<double> engine = 0.25;
  rd::EngineOptionalDouble ours;
  std::memset(&ours, 0, sizeof(ours));
  ours.value = 0.25;
  ours.engaged = true;
  CHECK_THAT(std::memcmp(&engine, &ours, 9) == 0);
  std::optional<double> none;
  unsigned char flag = 1;
  std::memcpy(&flag, reinterpret_cast<const unsigned char*>(&none) + 8, 1);
  CHECK_THAT(flag == 0);
}

void test_sets_through_a_real_vtable() {
  rd::ResetProbeForTesting();
  g_log.clear();
  FakeDecryptor dec;
  FakeReceiver r;
  r.decryptor = &dec;

  rd::Status s = rd::SetMinimumPlayoutDelay(
      &r, 250, rd::kMediaTypeVideo, &dec, &Log, rd::VtableAbi::kAbsolute);
  CHECK_THAT(s == rd::Status::kOk);
  CHECK_THAT(r.set_calls == 1);
  CHECK_THAT(r.last.has_value() && *r.last == 0.25);
  CHECK_THAT(r.media_type_calls == 1);
  CHECK_THAT(r.get_decryptor_calls == 1);
  CHECK_THAT(r.wrong_calls == 0);
  // The reference GetFrameDecryptor added was given back.
  CHECK_THAT(dec.refs == 1);
  CHECK_THAT(rd::ProbeHasPassed());

  // Zero gives the delay back: the engine's nullopt.
  s = rd::SetMinimumPlayoutDelay(
      &r, 0, rd::kMediaTypeVideo, &dec, &Log, rd::VtableAbi::kAbsolute);
  CHECK_THAT(s == rd::Status::kOk);
  CHECK_THAT(r.set_calls == 2);
  CHECK_THAT(!r.last.has_value());
  // The decryptor is asked once per process, the kind every time.
  CHECK_THAT(r.get_decryptor_calls == 1);
  CHECK_THAT(r.media_type_calls == 2);

  // Above the engine's own ceiling nothing larger is sent.
  s = rd::SetMinimumPlayoutDelay(
      &r, 60000, rd::kMediaTypeVideo, nullptr, &Log, rd::VtableAbi::kAbsolute);
  CHECK_THAT(s == rd::Status::kOk);
  CHECK_THAT(r.last.has_value() && *r.last == 10.0);
  CHECK_THAT(r.wrong_calls == 0);

  // An audio receiver, after the probe passed.
  FakeReceiver audio;
  audio.kind = cricket::MEDIA_TYPE_AUDIO;
  s = rd::SetMinimumPlayoutDelay(
      &audio, 120, rd::kMediaTypeAudio, nullptr, &Log,
      rd::VtableAbi::kAbsolute);
  CHECK_THAT(s == rd::Status::kOk);
  CHECK_THAT(audio.last.has_value() && *audio.last == 0.12);

  rd::ReleaseEngineRef(&r, rd::VtableAbi::kAbsolute);
  CHECK_THAT(r.release_calls == 1 && r.refs == 0);
}

void test_no_decryptor_is_not_a_failure() {
  rd::ResetProbeForTesting();
  FakeReceiver r;
  rd::Status s = rd::SetMinimumPlayoutDelay(
      &r, 250, rd::kMediaTypeVideo, nullptr, &Log, rd::VtableAbi::kAbsolute);
  CHECK_THAT(s == rd::Status::kNotAvailable);
  CHECK_THAT(r.set_calls == 0);
  CHECK_THAT(r.get_decryptor_calls == 0);
  CHECK_THAT(!rd::ProbeHasFailed() && !rd::ProbeHasPassed());
  CHECK_THAT(rd::SetMinimumPlayoutDelay(nullptr, 1, 0, nullptr, &Log) ==
             rd::Status::kNotAvailable);
}

void test_wrong_kind_stops_everything() {
  rd::ResetProbeForTesting();
  g_log.clear();
  FakeDecryptor dec;
  FakeReceiver r;
  r.decryptor = &dec;
  rd::Status s = rd::SetMinimumPlayoutDelay(
      &r, 250, rd::kMediaTypeAudio, &dec, &Log, rd::VtableAbi::kAbsolute);
  CHECK_THAT(s == rd::Status::kProbeFailed);
  CHECK_THAT(r.set_calls == 0);
  CHECK_THAT(r.get_decryptor_calls == 0);
  CHECK_THAT(rd::ProbeHasFailed());
  // Sticky, and said once.
  s = rd::SetMinimumPlayoutDelay(
      &r, 250, rd::kMediaTypeVideo, &dec, &Log, rd::VtableAbi::kAbsolute);
  CHECK_THAT(s == rd::Status::kProbeFailed);
  CHECK_THAT(r.set_calls == 0);
  CHECK_THAT(g_log.size() == 1);
}

void test_wrong_decryptor_stops_everything() {
  rd::ResetProbeForTesting();
  g_log.clear();
  FakeDecryptor attached;
  FakeDecryptor other;
  FakeReceiver r;
  r.decryptor = &other;
  rd::Status s = rd::SetMinimumPlayoutDelay(
      &r, 250, rd::kMediaTypeVideo, &attached, &Log, rd::VtableAbi::kAbsolute);
  CHECK_THAT(s == rd::Status::kProbeFailed);
  CHECK_THAT(r.set_calls == 0);
  // Neither decryptor was released by the probe: the one that came back is
  // not known to be one, and the attached one got no reference.
  CHECK_THAT(attached.refs == 1);
  CHECK_THAT(other.refs == 2);
  CHECK_THAT(g_log.size() == 1);

  // A receiver that returns nothing at all fails the same way.
  rd::ResetProbeForTesting();
  FakeReceiver empty;
  s = rd::SetMinimumPlayoutDelay(
      &empty, 250, rd::kMediaTypeVideo, &attached, &Log,
      rd::VtableAbi::kAbsolute);
  CHECK_THAT(s == rd::Status::kProbeFailed);
  CHECK_THAT(empty.set_calls == 0);
}

void test_a_pass_after_a_kind_mismatch_is_not_possible() {
  rd::ResetProbeForTesting();
  FakeDecryptor dec;
  FakeReceiver r;
  r.decryptor = &dec;
  CHECK_THAT(rd::SetMinimumPlayoutDelay(&r, 10, rd::kMediaTypeVideo, &dec,
                                        &Log, rd::VtableAbi::kAbsolute) ==
             rd::Status::kOk);
  // The probe passed; a later receiver whose kind is wrong still stops it.
  FakeReceiver audio;
  audio.kind = cricket::MEDIA_TYPE_AUDIO;
  CHECK_THAT(rd::SetMinimumPlayoutDelay(&audio, 10, rd::kMediaTypeVideo,
                                        nullptr, &Log,
                                        rd::VtableAbi::kAbsolute) ==
             rd::Status::kProbeFailed);
  CHECK_THAT(audio.set_calls == 0);
  CHECK_THAT(rd::SetMinimumPlayoutDelay(&r, 10, rd::kMediaTypeVideo, &dec,
                                        &Log, rd::VtableAbi::kAbsolute) ==
             rd::Status::kProbeFailed);
  CHECK_THAT(r.set_calls == 1);
}

// ---- The relative encoding (Android arm64), on a table built by hand ------

struct RelativeObject {
  const int32_t* vptr;
  int kind;
  void* decryptor;
  rd::EngineOptionalDouble last;
  int set_calls;
  int release_calls;
  int wrong_calls;
};

int RelWrong(const void* self) {
  ++const_cast<RelativeObject*>(static_cast<const RelativeObject*>(self))
        ->wrong_calls;
  return 0;
}
int RelMediaType(const void* self) {
  return static_cast<const RelativeObject*>(self)->kind;
}
int RelRelease(const void* self) {
  ++const_cast<RelativeObject*>(static_cast<const RelativeObject*>(self))
        ->release_calls;
  return 0;
}
void RelSetDelay(void* self, rd::EngineOptionalDouble delay) {
  auto* o = static_cast<RelativeObject*>(self);
  ++o->set_calls;
  o->last = delay;
}
rd::ReturnedRef RelGetDecryptor(const void* self) {
  rd::ReturnedRef ref;
  ref.ptr = static_cast<const RelativeObject*>(self)->decryptor;
  return ref;
}

int32_t g_relative_vtable[18];

int32_t RelativeSlot(const void* fn) {
  const std::ptrdiff_t delta =
      static_cast<const unsigned char*>(fn) -
      reinterpret_cast<const unsigned char*>(g_relative_vtable);
  if (delta > INT32_MAX || delta < INT32_MIN) {
    std::fprintf(stderr, "relative table out of 32-bit reach\n");
    std::abort();
  }
  return static_cast<int32_t>(delta);
}

void test_sets_through_a_relative_vtable() {
  for (int32_t& slot : g_relative_vtable) {
    slot = RelativeSlot(reinterpret_cast<const void*>(&RelWrong));
  }
  g_relative_vtable[rd::kSlotRelease] =
      RelativeSlot(reinterpret_cast<const void*>(&RelRelease));
  g_relative_vtable[rd::kSlotMediaType] =
      RelativeSlot(reinterpret_cast<const void*>(&RelMediaType));
  g_relative_vtable[rd::kSlotSetJitterBufferMinimumDelay] =
      RelativeSlot(reinterpret_cast<const void*>(&RelSetDelay));
  g_relative_vtable[rd::kSlotGetFrameDecryptor] =
      RelativeSlot(reinterpret_cast<const void*>(&RelGetDecryptor));

  rd::ResetProbeForTesting();
  FakeDecryptor dec;
  dec.refs = 2;  // ours and the one the fake "engine" returns
  RelativeObject o{};
  o.vptr = g_relative_vtable;
  o.kind = rd::kMediaTypeAudio;
  o.decryptor = static_cast<webrtc::FrameDecryptorInterface*>(&dec);

  rd::Status s = rd::SetMinimumPlayoutDelay(
      &o, 400, rd::kMediaTypeAudio, &dec, &Log, rd::VtableAbi::kRelative);
  CHECK_THAT(s == rd::Status::kOk);
  CHECK_THAT(o.set_calls == 1);
  CHECK_THAT(o.last.engaged && o.last.value == 0.4);
  CHECK_THAT(o.wrong_calls == 0);
  CHECK_THAT(dec.refs == 1);

  s = rd::SetMinimumPlayoutDelay(
      &o, 0, rd::kMediaTypeAudio, &dec, &Log, rd::VtableAbi::kRelative);
  CHECK_THAT(s == rd::Status::kOk);
  CHECK_THAT(o.set_calls == 2 && !o.last.engaged);

  rd::ReleaseEngineRef(&o, rd::VtableAbi::kRelative);
  CHECK_THAT(o.release_calls == 1);
  CHECK_THAT(o.wrong_calls == 0);
}

}  // namespace

int main() {
  test_layout_of_the_argument();
  test_sets_through_a_real_vtable();
  test_no_decryptor_is_not_a_failure();
  test_wrong_kind_stops_everything();
  test_wrong_decryptor_stops_everything();
  test_a_pass_after_a_kind_mismatch_is_not_possible();
  test_sets_through_a_relative_vtable();
  if (g_failures != 0) {
    std::fprintf(stderr, "test_receiver_delay: %d check(s) FAILED\n",
                 g_failures);
    return 1;
  }
  std::printf("test_receiver_delay: all 7 tests passed\n");
  return 0;
}

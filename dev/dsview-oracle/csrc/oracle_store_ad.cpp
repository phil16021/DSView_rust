/* C ABI around the original pv::data::DsoSnapshot and AnalogSnapshot
 * (compiled unchanged), for comparing the Rust DsoStore/AnalogStore.
 * See oracle.h. */

#include <malloc.h>
#include <string.h>
#include <vector>
#include <glib.h>

#include "pv/data/dsosnapshot.h"
#include "pv/data/analogsnapshot.h"

extern "C" {
#include "oracle.h"
}

using pv::data::AnalogSnapshot;
using pv::data::DsoSnapshot;

/* Both classes befriend a `Basic` test class; defined here for read and
 * zero-fill access (C++ leaves malloc'ed buffers uninitialised). */
namespace DsoSnapshotTest {
class Basic {
public:
    static void zero(DsoSnapshot &s)
    {
        for (uint8_t *p : s._ch_data)
            memset(p, 0, s._total_sample_count + 1);
    }
    static const uint8_t *buffer(DsoSnapshot &s, int order)
    {
        return order < (int)s._ch_data.size() ? s._ch_data[order] : NULL;
    }
    static void counts(DsoSnapshot &s, struct osdso_counts *c)
    {
        c->sample_count = s._sample_count;
        c->total_sample_count = s._total_sample_count;
        c->ring_sample_count = s._ring_sample_count;
        c->channel_num = s._channel_num;
        c->last_ended = s._last_ended;
        c->memory_failed = s._memory_failed;
        c->is_file = s._is_file;
        c->instant = s._instant;
        c->out_of_range = s._data_out_off_range;
        c->block_num = s._sample_count ? s.get_block_num() : 0;
        c->n_orders = s._ch_data.size();
    }
};
}

namespace AnalogSnapshotTest {
class Basic {
public:
    /* Envelope rings get zeros, and the allocator slack after them gets
     * neutral (min 0xff, max 0) entries: building the next level reads one
     * entry past a ring (F12), which then cannot change the result. */
    static void zero(AnalogSnapshot &s)
    {
        if (s._data)
            memset(s._data, 0, s._capacity);
        for (auto &levels : s._envelope_levels)
            for (auto &e : levels)
                if (e.samples) {
                    size_t used = e.count * sizeof(AnalogSnapshot::EnvelopeSample);
                    size_t usable = malloc_usable_size(e.samples);
                    memset(e.samples, 0, used);
                    for (size_t b = used; b + 1 < usable; b += 2) {
                        ((uint8_t *)e.samples)[b] = 0xff;
                        ((uint8_t *)e.samples)[b + 1] = 0;
                    }
                }
    }
    static int slack(AnalogSnapshot &s, int probe, int level)
    {
        auto &e = s._envelope_levels[probe][level];
        if (!e.samples)
            return 0;
        return malloc_usable_size(e.samples) - e.count * sizeof(AnalogSnapshot::EnvelopeSample);
    }
    static void counts(AnalogSnapshot &s, struct osana_counts *c)
    {
        c->sample_count = s._sample_count;
        c->total_sample_count = s._total_sample_count;
        c->ring_sample_count = s._ring_sample_count;
        c->capacity = s._capacity;
        c->channel_num = s._channel_num;
        c->unit_bytes = s._unit_bytes;
        c->unit_pitch = s._unit_pitch;
        c->last_ended = s._last_ended;
        c->memory_failed = s._memory_failed;
        c->block_num = s._sample_count ? s.get_block_num() : 0;
    }
    static void envelope(AnalogSnapshot &s, int probe, int level, uint64_t *len, uint64_t *ring_len,
                         uint64_t *count, const uint8_t **samples)
    {
        auto &e = s._envelope_levels[probe][level];
        *len = e.length;
        *ring_len = e.ring_length;
        *count = e.count;
        *samples = (const uint8_t *)e.samples;
    }
};
}

struct osdso {
    DsoSnapshot snap;
};

struct osana {
    AnalogSnapshot snap;
};

static GSList *channel_list(std::vector<sr_channel> &chans, int type, const uint16_t *idx,
                            const int *enabled, int n)
{
    chans.resize(n);
    GSList *list = NULL;
    for (int i = 0; i < n; i++) {
        chans[i] = {};
        chans[i].index = idx[i];
        chans[i].type = type;
        chans[i].enabled = enabled[i] ? TRUE : FALSE;
        list = g_slist_append(list, &chans[i]);
    }
    return list;
}

extern "C" {

/* ------------------------------------------------------------ DSO */

osdso *osdso_new(void)
{
    osdso *s = new osdso();
    s->snap.clear();
    return s;
}

void osdso_free(osdso *s) { delete s; }
void osdso_clear(osdso *s) { s->snap.clear(); }
void osdso_init(osdso *s) { s->snap.init(); }
void osdso_capture_ended(osdso *s) { s->snap.capture_ended(); }
void osdso_set_ref_range(osdso *s, uint32_t ref_max, uint32_t ref_min) { s->snap.set_ref_range(ref_max, ref_min); }
void osdso_zero(osdso *s) { DsoSnapshotTest::Basic::zero(s->snap); }
void osdso_counts(osdso *s, struct osdso_counts *c) { DsoSnapshotTest::Basic::counts(s->snap, c); }
uint64_t osdso_block_size(osdso *s, int block) { return s->snap.get_block_size(block); }
int osdso_has_data(osdso *s, int sig) { return s->snap.has_data(sig); }
const uint8_t *osdso_buffer(osdso *s, int order) { return DsoSnapshotTest::Basic::buffer(s->snap, order); }

const uint8_t *osdso_get_samples(osdso *s, uint64_t start, int sig)
{
    return s->snap.get_samples(start, start, sig);
}

int osdso_max_min(osdso *s, int order, uint8_t *maxv, uint8_t *minv)
{
    return s->snap.get_max_min_value(*maxv, *minv, order);
}

void osdso_first_payload(osdso *s, const uint8_t *data, int num_samples, uint64_t total,
                         const uint16_t *idx, const int *enabled, int n, int instant, int is_file,
                         int samplerate_tog)
{
    std::vector<sr_channel> chans;
    GSList *list = channel_list(chans, SR_CHANNEL_DSO, idx, enabled, n);
    sr_datafeed_dso dso = {};
    dso.num_samples = num_samples;
    dso.data = (void *)data;
    dso.samplerate_tog = samplerate_tog != 0;
    s->snap.first_payload(dso, total, list, instant != 0, is_file != 0);
    g_slist_free(list);
}

void osdso_append(osdso *s, const uint8_t *data, int num_samples, int samplerate_tog)
{
    sr_datafeed_dso dso = {};
    dso.num_samples = num_samples;
    dso.data = (void *)data;
    dso.samplerate_tog = samplerate_tog != 0;
    s->snap.append_payload(dso);
}

void osdso_enable_envelope(osdso *s, int enable) { s->snap.enable_envelope(enable != 0); }

void osdso_envelope_section(osdso *s, uint64_t start, uint64_t end, float min_length, int probe,
                            uint64_t *out_start, unsigned *out_scale, uint64_t *out_length,
                            const uint8_t **out_samples)
{
    DsoSnapshot::EnvelopeSection e = {};
    s->snap.get_envelope_section(e, start, end, min_length, probe);
    *out_start = e.start;
    *out_scale = e.scale;
    *out_length = e.length;
    *out_samples = (const uint8_t *)e.samples;
}

/* ------------------------------------------------------------ analog */

osana *osana_new(void)
{
    osana *s = new osana();
    s->snap.clear();
    return s;
}

void osana_free(osana *s) { delete s; }
void osana_clear(osana *s) { s->snap.clear(); }
void osana_init(osana *s) { s->snap.init(); }
void osana_capture_ended(osana *s) { s->snap.capture_ended(); }
void osana_zero(osana *s) { AnalogSnapshotTest::Basic::zero(s->snap); }
void osana_counts(osana *s, struct osana_counts *c) { AnalogSnapshotTest::Basic::counts(s->snap, c); }
uint64_t osana_block_size(osana *s, int block) { return s->snap.get_block_size(block); }
int osana_has_data(osana *s, int sig) { return s->snap.has_data(sig); }
int osana_has_enabled_channel(osana *s, int sig) { return s->snap.has_enabled_channel(sig); }
int osana_ch_order(osana *s, int sig) { return s->snap.get_ch_order(sig); }
const uint8_t *osana_data(osana *s) { return (const uint8_t *)s->snap.get_data(); }
int osana_envelope_slack(osana *s, int probe, int level) { return AnalogSnapshotTest::Basic::slack(s->snap, probe, level); }

void osana_envelope(osana *s, int probe, int level, uint64_t *len, uint64_t *ring_len, uint64_t *count,
                    const uint8_t **samples)
{
    AnalogSnapshotTest::Basic::envelope(s->snap, probe, level, len, ring_len, count, samples);
}

void osana_section(osana *s, uint64_t start, int64_t count, float min_length, int probe,
                   struct osana_section *out)
{
    AnalogSnapshot::EnvelopeSection e = {};
    s->snap.get_envelope_section(e, start, count, min_length, probe);
    out->start = e.start;
    out->scale = e.scale;
    out->length = e.length;
    out->samples_num = e.samples_num;
    out->samples = (const uint8_t *)e.samples;
}

void osana_first_payload(osana *s, const uint8_t *data, int num_samples, uint8_t unit_bits,
                         uint16_t unit_pitch, uint64_t total, const uint16_t *idx, const int *enabled, int n)
{
    std::vector<sr_channel> chans;
    GSList *list = channel_list(chans, SR_CHANNEL_ANALOG, idx, enabled, n);
    sr_datafeed_analog analog = {};
    analog.num_samples = num_samples;
    analog.unit_bits = unit_bits;
    analog.unit_pitch = unit_pitch;
    analog.data = (void *)data;
    s->snap.first_payload(analog, total, list);
    g_slist_free(list);
}

void osana_append(osana *s, const uint8_t *data, int num_samples, uint16_t unit_pitch)
{
    sr_datafeed_analog analog = {};
    analog.num_samples = num_samples;
    analog.unit_pitch = unit_pitch;
    analog.data = (void *)data;
    s->snap.append_payload(analog);
}

} /* extern "C" */

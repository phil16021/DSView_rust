/* C ABI around the original pv::data::LogicSnapshot (DSView/pv/data/
 * logicsnapshot.cpp, compiled unchanged against Qt Core) so the Rust
 * LogicStore can be compared call by call. See oracle.h. */

#include <map>
#include <vector>
#include <glib.h>
#include <QString>

#include "pv/data/logicsnapshot.h"
#include "log/xlog.h"

extern "C" {
#include "oracle.h"
}

/* DSView's logger; NULL makes every dsv_* log call a no-op. */
xlog_writer *dsv_log = nullptr;

using pv::data::LogicSnapshot;

/* LogicSnapshot befriends LogicSnapshotTest::Pow2 (its old unit tests);
 * defining it here gives the oracle read access to the internal state. */
namespace LogicSnapshotTest {
class Pow2 {
public:
    static void internals(LogicSnapshot &s, struct ostore_internals *o)
    {
        o->byte_fraction = s._byte_fraction;
        o->ch_fraction = s._ch_fraction;
        o->dest_order = o->dest_root = o->dest_pos = o->dest_byte = -1;
        if (s._dest_ptr != NULL) {
            for (size_t c = 0; c < s._ch_data.size(); c++)
                for (size_t r = 0; r < s._ch_data[c].size(); r++)
                    for (int p = 0; p < 64; p++) {
                        uint8_t *l = (uint8_t *)s._ch_data[c][r].lbp[p];
                        if (l != NULL && s._dest_ptr >= l && s._dest_ptr < l + LogicSnapshot::LeafBlockSpace) {
                            o->dest_order = (int64_t)c;
                            o->dest_root = (int64_t)r;
                            o->dest_pos = p;
                            o->dest_byte = s._dest_ptr - l;
                        }
                    }
            if (o->dest_order < 0)
                o->dest_byte = -2; /* points outside every live leaf */
        }
        for (int i = 0; i < CHANNEL_MAX_COUNT; i++) {
            o->last_sample[i] = s._last_sample[i];
            o->last_calc_count[i] = s._last_calc_count[i];
            o->cur_ref_root[i] = s._cur_ref_block_indexs[i].root_index;
            o->cur_ref_lbp[i] = s._cur_ref_block_indexs[i].lbp_index;
        }
        o->is_loop = s._is_loop;
        o->able_free = s._able_free;
        o->kept_leaves = s._free_block_list.size();
        o->lst_free_block_index = s._lst_free_block_index;
        o->n_orders = s._ch_data.size();
    }

    static int roots(LogicSnapshot &s, int order)
    {
        return s._ch_data[order].size();
    }

    static void root(LogicSnapshot &s, int order, int root, uint64_t *bits, const uint64_t **leaves)
    {
        auto &rn = s._ch_data[order][root];
        bits[0] = rn.tog;
        bits[1] = rn.first;
        bits[2] = rn.last;
        for (int p = 0; p < 64; p++)
            leaves[p] = (const uint64_t *)rn.lbp[p];
    }
};
}

struct oracle_store {
    LogicSnapshot snap;
};

extern "C" {

oracle_store *ostore_new(void)
{
    return new oracle_store();
}

void ostore_free(oracle_store *s)
{
    delete s;
}

void ostore_set_loop(oracle_store *s, int loop)
{
    s->snap.set_loop(loop != 0);
}

static sr_datafeed_logic make_logic(const uint8_t *data, uint64_t len)
{
    sr_datafeed_logic logic = {};
    logic.length = len;
    logic.format = LA_CROSS_DATA;
    logic.data = (void *)data;
    return logic;
}

void ostore_first_payload(oracle_store *s, const uint8_t *data, uint64_t len, uint64_t total,
                          const uint16_t *ch, int n_ch, int able_free)
{
    std::vector<sr_channel> chans(n_ch);
    GSList *list = NULL;
    for (int i = 0; i < n_ch; i++) {
        chans[i] = {};
        chans[i].index = ch[i];
        chans[i].type = SR_CHANNEL_LOGIC;
        chans[i].enabled = TRUE;
        list = g_slist_append(list, &chans[i]);
    }
    sr_datafeed_logic logic = make_logic(data, len);
    s->snap.first_payload(logic, total, list, able_free != 0);
    g_slist_free(list);
}

void ostore_append(oracle_store *s, const uint8_t *data, uint64_t len)
{
    sr_datafeed_logic logic = make_logic(data, len);
    s->snap.append_payload(logic);
}

void ostore_capture_ended(oracle_store *s)
{
    s->snap.capture_ended();
}

void ostore_clear(oracle_store *s)
{
    s->snap.clear();
}

void ostore_counts(oracle_store *s, struct ostore_counts *c)
{
    LogicSnapshot &n = s->snap;
    c->sample_count = n.get_sample_count();
    c->ring_sample_count = n.get_ring_sample_count();
    c->total_sample_count = n.get_total_sample_count();
    c->loop_offset = n.get_loop_offset();
    c->ring_start = n.get_ring_start();
    c->ring_end = n.get_ring_end();
    c->block_num = n.get_block_num();
    c->last_ended = n.last_ended();
    c->channel_num = n.get_channel_num();
}

uint64_t ostore_block_size(oracle_store *s, int block)
{
    return s->snap.get_block_size(block);
}

const uint8_t *ostore_block_buf(oracle_store *s, int block, int sig, int *sample)
{
    bool flag = false;
    const uint8_t *p = s->snap.get_block_buf(block, sig, flag);
    *sample = flag;
    return p;
}

const uint8_t *ostore_get_samples(oracle_store *s, uint64_t start, uint64_t *end, int sig, void **lbp)
{
    uint64_t e = 0;
    const uint8_t *p = s->snap.get_samples(start, e, sig, lbp);
    *end = e;
    return p;
}

int ostore_get_sample(oracle_store *s, uint64_t index, int sig)
{
    return s->snap.get_sample(index, sig);
}

int ostore_has_data(oracle_store *s, int sig)
{
    return s->snap.has_data(sig);
}

int ostore_display_edges(oracle_store *s, uint64_t start, uint64_t end, uint16_t width,
                         uint16_t max_togs, double pixels_offset, double min_length, uint16_t sig,
                         uint8_t *edges, int *n_edges, uint16_t *tog_pos, uint8_t *tog_val,
                         int *n_togs)
{
    std::vector<std::pair<bool, bool>> e;
    std::vector<std::pair<uint16_t, bool>> t;
    bool first = s->snap.get_display_edges(e, t, start, end, width, max_togs, pixels_offset,
                                           min_length, sig);
    /* Callers size the buffers for `width` edges and `max_togs` + 1 toggles. */
    *n_edges = (int)e.size();
    for (size_t i = 0; i < e.size(); i++)
        edges[i] = (e[i].first ? 2 : 0) | (e[i].second ? 1 : 0);
    *n_togs = (int)t.size();
    for (size_t i = 0; i < t.size(); i++) {
        tog_pos[i] = t[i].first;
        tog_val[i] = t[i].second;
    }
    return first;
}

int ostore_nxt_edge(oracle_store *s, uint64_t *index, int last_sample, uint64_t end,
                    double min_length, int sig)
{
    return s->snap.get_nxt_edge(*index, last_sample != 0, end, min_length, sig);
}

int ostore_pre_edge(oracle_store *s, uint64_t *index, int last_sample, double min_length, int sig)
{
    return s->snap.get_pre_edge(*index, last_sample != 0, min_length, sig);
}

int ostore_pattern_search(oracle_store *s, int64_t start, int64_t end, int64_t *index,
                          const uint16_t *chs, const char *flags, int n, int is_next)
{
    std::map<uint16_t, QString> pattern;
    for (int i = 0; i < n; i++)
        pattern[chs[i]] = QString(QChar(flags[i]));
    int64_t idx = *index;
    bool r = s->snap.pattern_search(start, end, idx, pattern, is_next != 0);
    *index = idx;
    return r;
}

int ostore_block_index_with_sample(oracle_store *s, uint64_t sample, uint64_t *offset)
{
    return s->snap.get_block_index_with_sample(sample, offset);
}

void ostore_decode_end(oracle_store *s)
{
    s->snap.decode_end();
}

void ostore_free_decode_lpb(oracle_store *s, void *lbp)
{
    s->snap.free_decode_lpb(lbp);
}

void ostore_internals(oracle_store *s, struct ostore_internals *o)
{
    LogicSnapshotTest::Pow2::internals(s->snap, o);
}

int ostore_roots(oracle_store *s, int order)
{
    return LogicSnapshotTest::Pow2::roots(s->snap, order);
}

void ostore_root(oracle_store *s, int order, int root, uint64_t *bits, const uint64_t **leaves)
{
    LogicSnapshotTest::Pow2::root(s->snap, order, root, bits, leaves);
}

} /* extern "C" */

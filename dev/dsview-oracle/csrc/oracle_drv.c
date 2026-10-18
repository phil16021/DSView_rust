/*
 * Driver oracle: drives libsigrok4DSL through its public ds_* API exactly as
 * DSView does (activate, config get/set/list, start/stop collect), so the C
 * demo driver and the Rust one behind rs_driver.c (DSVIEW_IMPL_DRV_DEMO=rust)
 * can be compared call by call. Values are printed with g_variant_print;
 * sr_list_item lists handed out by pointer are printed item by item.
 */

#include <glib.h>
#include <string.h>
#include <stdio.h>
#include "libsigrok.h"
#include "libsigrok-internal.h"

#include "oracle.h"

static GMutex g_lock;
static GCond g_cond;
static gboolean g_ended;
static int g_data_packets;
static int g_stop_after;
static oracle_drv_packet_cb g_pcb;
static void *g_puser;

static guint enabled_count(GSList *probes)
{
    guint n = 0;
    for (GSList *l = probes; l; l = l->next)
        n += ((const struct sr_channel *)l->data)->enabled != 0;
    return n;
}

static void on_datafeed(const struct sr_dev_inst *sdi, const struct sr_datafeed_packet *packet)
{
    struct oracle_drv_packet p;
    (void)sdi;

    memset(&p, 0, sizeof(p));
    p.type = packet->type;
    p.status = packet->status;
    switch (packet->type) {
    case SR_DF_HEADER:
        p.feed_version = ((const struct sr_datafeed_header *)packet->payload)->feed_version;
        break;
    case SR_DF_TRIGGER:
        p.trig_real_pos = ((const struct ds_trigger_pos *)packet->payload)->real_pos;
        break;
    case SR_DF_LOGIC: {
        const struct sr_datafeed_logic *l = packet->payload;
        p.data = l->data;
        p.len = l->length;
        p.format = l->format;
        p.index = l->index;
        p.order = l->order;
        p.unitsize = l->unitsize;
        p.data_error = l->data_error;
        p.error_pattern = l->error_pattern;
        break;
    }
    case SR_DF_DSO: {
        const struct sr_datafeed_dso *d = packet->payload;
        p.data = d->data;
        p.num_samples = d->num_samples;
        /* DsoSnapshot reads num_samples per enabled channel */
        p.len = (uint64_t)d->num_samples * enabled_count(d->probes);
        p.mq = d->mq;
        p.unit = d->unit;
        p.mqflags = d->mqflags;
        p.samplerate_tog = d->samplerate_tog;
        p.trig_flag = d->trig_flag;
        p.trig_ch = d->trig_ch;
        p.nprobes = g_slist_length(d->probes);
        break;
    }
    case SR_DF_ANALOG: {
        const struct sr_datafeed_analog *a = packet->payload;
        int unit = a->unit_bits > 8 ? 2 : 1;
        p.data = a->data;
        p.num_samples = a->num_samples;
        p.len = (uint64_t)a->num_samples * g_slist_length(a->probes) * unit;
        p.mq = a->mq;
        p.unit = a->unit;
        p.mqflags = a->mqflags;
        p.unit_bits = a->unit_bits;
        p.unit_pitch = a->unit_pitch;
        p.nprobes = g_slist_length(a->probes);
        break;
    }
    }
    g_pcb(g_puser, &p);

    if (packet->type == SR_DF_LOGIC || packet->type == SR_DF_DSO || packet->type == SR_DF_ANALOG) {
        /* Stop from the collect thread itself, between two callbacks, so
         * the number of packets before the stop is deterministic. */
        if (++g_data_packets == g_stop_after)
            sr_session_stop();
    }
}

static void on_event(int event)
{
    if (event == DS_EV_COLLECT_TASK_END || event == DS_EV_COLLECT_TASK_END_BY_ERROR ||
        event == DS_EV_COLLECT_TASK_END_BY_DETACHED) {
        g_mutex_lock(&g_lock);
        g_ended = TRUE;
        g_cond_signal(&g_cond);
        g_mutex_unlock(&g_lock);
    }
}

int oracle_drv_init(const char *user_dir, int log_level)
{
    ds_log_level(log_level);
    ds_set_user_data_dir(user_dir);
    {
        /* firmware and FPGA bitstreams: DSView/res next to the demo files */
        char *res = g_build_filename(user_dir, "res", NULL);
        ds_set_firmware_resource_dir(res);
        g_free(res);
    }
    ds_set_datafeed_callback(on_datafeed);
    ds_set_event_callback(on_event);
    return ds_lib_init();
}

void oracle_drv_exit(void)
{
    ds_release_actived_device();
    ds_lib_exit();
}

/* Activates the device named `name` (the demo devices are the first ones). */
int oracle_drv_activate(const char *name)
{
    struct ds_device_base_info *list = NULL;
    int count = 0, ret = SR_ERR_CALL_STATUS;

    if (ds_get_device_list(&list, &count) != SR_OK)
        return SR_ERR;
    for (int i = 0; i < count; i++) {
        if (strcmp(list[i].name, name) == 0) {
            ret = ds_active_device(list[i].handle);
            break;
        }
    }
    g_free(list);
    return ret;
}

/* SigSession::set_file: a device for the file, activated as the last one. */
int oracle_drv_open_file(const char *path)
{
    int ret = ds_device_from_file(path);
    if (ret != SR_OK)
        return ret;
    return ds_active_device_by_index(-1);
}

static const struct sr_channel *nth_channel(int ch)
{
    GSList *l = ch >= 0 ? g_slist_nth(ds_get_actived_device_channels(), ch) : NULL;
    return l ? l->data : NULL;
}

static int is_list_key(int key)
{
    return key == SR_CONF_CHANNEL_MODE || key == SR_CONF_OPERATION_MODE ||
           key == SR_CONF_BUFFER_OPTIONS || key == SR_CONF_THRESHOLD ||
           key == SR_CONF_FILTER || key == SR_CONF_BANDWIDTH_LIMIT;
}

static char *print_value(int key, GVariant *v, int list)
{
    if (v == NULL)
        return g_strdup("null");
    if (list && is_list_key(key) && g_variant_is_of_type(v, G_VARIANT_TYPE_UINT64)) {
        const struct sr_list_item *it = (const struct sr_list_item *)(uintptr_t)g_variant_get_uint64(v);
        GString *s = g_string_new("list[");
        for (; it && it->id >= 0; it++)
            g_string_append_printf(s, "%d:%s,", it->id, it->name ? it->name : "(null)");
        g_string_append(s, "]");
        return g_string_free(s, FALSE);
    }
    return g_variant_print(v, TRUE);
}

/* "<ret> <value>" for config_get on channel position `ch` (-1: none). */
char *oracle_drv_get(int key, int ch)
{
    GVariant *v = NULL;
    int ret = ds_get_actived_device_config(nth_channel(ch), NULL, key, &v);
    char *val = ret == SR_OK ? print_value(key, v, 0) : g_strdup("-");
    char *out = g_strdup_printf("%d %s", ret, val);
    g_free(val);
    if (v)
        g_variant_unref(v);
    return out;
}

char *oracle_drv_list(int key)
{
    GVariant *v = NULL;
    int ret = ds_get_actived_device_config_list(NULL, key, &v);
    char *val = ret == SR_OK ? print_value(key, v, 1) : g_strdup("-");
    char *out = g_strdup_printf("%d %s", ret, val);
    g_free(val);
    if (v)
        g_variant_unref(v);
    return out;
}

/* config_set with a value in GVariant text format ("uint64 5", "'sine'"). */
int oracle_drv_set(int key, int ch, const char *text)
{
    GError *err = NULL;
    GVariant *v = g_variant_parse(NULL, text, NULL, NULL, &err);
    if (v == NULL) {
        g_error_free(err);
        return -100;
    }
    return ds_set_actived_device_config(nth_channel(ch), NULL, key, v);
}

int oracle_drv_enable(int ch, int enable)
{
    return ds_enable_device_channel_index(ch, enable);
}

/* Device state: mode, status, path, then one line per channel. */
char *oracle_drv_state(void)
{
    struct ds_device_full_info info;
    GString *s = g_string_new(NULL);

    memset(&info, 0, sizeof(info));
    ds_get_actived_device_info(&info);
    g_string_append_printf(s, "mode=%d name=%s path=%s", ds_get_actived_device_mode(),
                           info.name, info.path);
    for (GSList *l = ds_get_actived_device_channels(); l; l = l->next) {
        const struct sr_channel *c = l->data;
        g_string_append_printf(s,
            "\n ch index=%u type=%d en=%d name=%s trig=%s bits=%u vdiv=%llu vfactor=%llu "
            "offset=%u zero=%u hw=%u vpos=%u coupling=%u trig_value=%u map_default=%d "
            "map_unit=%s map=%g..%g comb=%d/%d/%d digi=%u fgain=%.17g,%.17g,%.17g,%.17g "
            "comb_fgain=%.17g,%.17g,%.17g,%.17g",
            c->index, c->type, c->enabled, c->name ? c->name : "(null)",
            c->trigger ? c->trigger : "(null)", c->bits, (unsigned long long)c->vdiv,
            (unsigned long long)c->vfactor, c->offset, c->zero_offset, c->hw_offset,
            c->vpos_trans, c->coupling, c->trig_value, c->map_default,
            c->map_unit ? c->map_unit : "(null)", c->map_min, c->map_max,
            c->comb_diff_top, c->comb_diff_bom, c->comb_comp, c->digi_fgain,
            c->cali_fgain0, c->cali_fgain1, c->cali_fgain2, c->cali_fgain3,
            c->cali_comb_fgain0, c->cali_comb_fgain1, c->cali_comb_fgain2,
            c->cali_comb_fgain3);
    }
    {
        const GSList *modes = ds_get_actived_device_mode_list();
        g_string_append(s, "\n modes");
        for (const GSList *l = modes; l; l = l->next) {
            const struct sr_dev_mode *m = l->data;
            g_string_append_printf(s, " %d:%s", m->mode, m->acronym);
        }
    }
    {
        struct sr_status st;
        int ret;
        memset(&st, 0, sizeof(st));
        ret = ds_get_actived_device_status(&st, FALSE);
        g_string_append_printf(s, "\n status %d", ret);
#define F(f) g_string_append_printf(s, " " #f "=%llu", (unsigned long long)st.f)
        F(trig_hit); F(captured_cnt3); F(captured_cnt2); F(captured_cnt1); F(captured_cnt0);
        F(pkt_id); F(vlen); F(stream_mode); F(measure_valid); F(sample_divider);
        F(sample_divider_tog); F(trig_flag); F(trig_ch); F(trig_offset);
        F(ch0_max); F(ch0_min); F(ch0_cyc_cnt); F(ch0_cyc_tlen); F(ch0_cyc_plen); F(ch0_cyc_llen);
        F(ch0_level_valid); F(ch0_plevel); F(ch0_low_level); F(ch0_high_level); F(ch0_cyc_rlen);
        F(ch0_cyc_flen); F(ch0_acc_square); F(ch0_acc_mean); F(ch0_acc_mean_p1);
        F(ch0_acc_mean_p2); F(ch0_acc_mean_p3);
        F(ch1_max); F(ch1_min); F(ch1_cyc_cnt); F(ch1_cyc_tlen); F(ch1_cyc_plen); F(ch1_cyc_llen);
        F(ch1_level_valid); F(ch1_plevel); F(ch1_low_level); F(ch1_high_level); F(ch1_cyc_rlen);
        F(ch1_cyc_flen); F(ch1_acc_square); F(ch1_acc_mean); F(ch1_acc_mean_p1);
        F(ch1_acc_mean_p2); F(ch1_acc_mean_p3);
#undef F
    }
    return g_string_free(s, FALSE);
}

/* Captures until SR_DF_END and the collect task's end, stopping the session
 * after `stop_after` data packets (0: never). Returns 0, or -2 on timeout. */
int oracle_drv_capture(int stop_after, oracle_drv_packet_cb cb, void *user)
{
    int ret;
    gint64 deadline;

    g_pcb = cb;
    g_puser = user;
    g_data_packets = 0;
    g_stop_after = stop_after;
    g_ended = FALSE;

    ret = ds_start_collect();
    if (ret != SR_OK)
        return ret;

    g_mutex_lock(&g_lock);
    /* captures that send nothing (F19) end here, after 10 s (or
     * ORACLE_CAPTURE_DEADLINE_S); the rest take well under a second, and
     * simulated USB captures are stopped by their transfer budget */
    {
        const char *s = g_getenv("ORACLE_CAPTURE_DEADLINE_S");
        gint64 secs = s ? g_ascii_strtoll(s, NULL, 10) : 0;
        deadline = g_get_monotonic_time() + (secs > 0 ? secs : 10) * G_TIME_SPAN_SECOND;
    }
    while (!g_ended) {
        if (!g_cond_wait_until(&g_cond, &g_lock, deadline))
            break;
    }
    gboolean ended = g_ended;
    g_mutex_unlock(&g_lock);
    if (ds_is_collecting())
        ds_stop_collect();
    return ended ? 0 : -2;
}

/* The demo driver's clock (demo.c is built with g_timer_*, g_usleep, rand
 * and srand mapped onto these), backed by the Rust virtual clock. */
struct oracle_timer {
    int64_t start;
};

void *oracle_timer_new(void)
{
    struct oracle_timer *t = g_malloc0(sizeof(*t));
    t->start = oracle_clock_now_us();
    return t;
}

void oracle_timer_start(void *t)
{
    if (t)
        ((struct oracle_timer *)t)->start = oracle_clock_now_us();
}

double oracle_timer_elapsed(void *t, gulong *us)
{
    int64_t d;
    (void)us;
    if (!t)
        return 0;
    d = oracle_clock_now_us() - ((struct oracle_timer *)t)->start;
    return (double)d / 1e6;
}

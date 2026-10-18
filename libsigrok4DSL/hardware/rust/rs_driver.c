/*
 * Drivers backed by the Rust core (crates/dsview-drivers via dsv_drv_* in
 * dsview_ffi.h), used instead of their C counterparts when
 * DSVIEW_IMPL_DRV_<NAME>=rust (or DSVIEW_IMPL=rust): the demo driver
 * (DRV_DEMO) and DSLogic driver (DRV_DSLOGIC), both in sr_driver_list, and
 * the file driver (DRV_FILE, in sr_new_virtual_device). Transitional:
 * removed with the C drivers.
 *
 * The struct sr_dev_inst the library and the GUI use stays in C. Its channel
 * list mirrors the Rust device's: fields the GUI writes directly (enabled,
 * name, ...) are copied in before every call, and the list is copied back
 * after calls that may change it, rebuilt only where the C driver would
 * free and recreate its channels (same pointer lifetimes for the GUI).
 */

#ifdef DSVIEW_RUST_CORE

#include "../../libsigrok-internal.h"
#include <glib.h>
#include <string.h>
#include "../../log.h"
#include "dsview_ffi.h"

#undef LOG_PREFIX
#define LOG_PREFIX "rs-driver: "

extern char DS_USR_PATH[500];
extern char DS_RES_PATH[500];
extern struct ds_trigger *trigger;

_Static_assert(sizeof(struct ds_trigger) == 1284, "struct ds_trigger layout (dsview_core::Trigger)");

_Static_assert(sizeof(struct sr_status) == 160, "struct sr_status layout (dsview_core::Status)");

/* sdi->priv; g_free'd by sr_dev_inst_free */
struct rs_dev {
    struct dsv_device *h;
    uint64_t generation;
    GSList *modes;
};

static struct dsv_device *H(const struct sr_dev_inst *sdi)
{
    return ((struct rs_dev *)sdi->priv)->h;
}

static void rs_log(int32_t level, const char *msg)
{
    switch (level) {
    case 1: sr_err("%s", msg); break;
    case 2: sr_warn("%s", msg); break;
    case 3: sr_info("%s", msg); break;
    case 4: sr_dbg("%s", msg); break;
    default: sr_detail("%s", msg); break;
    }
}

/* ------------------------------------------------------- channel mirror */

static void put_channels(const struct sr_dev_inst *sdi)
{
    struct dsv_channel c;
    uint32_t i = 0;

    dsv_drv_set_status(H(sdi), sdi->status);
    for (GSList *l = sdi->channels; l; l = l->next, i++) {
        const struct sr_channel *p = l->data;
        c.index = p->index;
        c.ty = p->type;
        c.enabled = p->enabled;
        c.name = p->name;
        c.trigger = p->trigger;
        c.bits = p->bits;
        c.vdiv = p->vdiv;
        c.vfactor = p->vfactor;
        c.offset = p->offset;
        c.zero_offset = p->zero_offset;
        c.hw_offset = p->hw_offset;
        c.vpos_trans = p->vpos_trans;
        c.coupling = p->coupling;
        c.trig_value = p->trig_value;
        c.comb_diff_top = p->comb_diff_top;
        c.comb_diff_bom = p->comb_diff_bom;
        c.comb_comp = p->comb_comp;
        c.digi_fgain = p->digi_fgain;
        c.cali_fgain[0] = p->cali_fgain0;
        c.cali_fgain[1] = p->cali_fgain1;
        c.cali_fgain[2] = p->cali_fgain2;
        c.cali_fgain[3] = p->cali_fgain3;
        c.cali_comb_fgain[0] = p->cali_comb_fgain0;
        c.cali_comb_fgain[1] = p->cali_comb_fgain1;
        c.cali_comb_fgain[2] = p->cali_comb_fgain2;
        c.cali_comb_fgain[3] = p->cali_comb_fgain3;
        c.map_default = p->map_default;
        c.map_unit = p->map_unit;
        c.map_min = p->map_min;
        c.map_max = p->map_max;
        dsv_drv_channel_put(H(sdi), i, &c);
    }
}

static void set_str(char **dst, const char *src)
{
    if (src == NULL) {
        safe_free(*dst);
    } else if (*dst == NULL || strcmp(*dst, src) != 0) {
        g_free(*dst);
        *dst = g_strdup(src);
    }
}

static void fill_channel(struct sr_channel *p, const struct dsv_channel *c)
{
    p->index = c->index;
    p->type = c->ty;
    p->enabled = c->enabled;
    set_str(&p->name, c->name);
    set_str(&p->trigger, c->trigger);
    p->bits = c->bits;
    p->vdiv = c->vdiv;
    p->vfactor = c->vfactor;
    p->offset = c->offset;
    p->zero_offset = c->zero_offset;
    p->hw_offset = c->hw_offset;
    p->vpos_trans = c->vpos_trans;
    p->coupling = c->coupling;
    p->trig_value = c->trig_value;
    p->comb_diff_top = c->comb_diff_top;
    p->comb_diff_bom = c->comb_diff_bom;
    p->comb_comp = c->comb_comp;
    p->digi_fgain = c->digi_fgain;
    p->cali_fgain0 = c->cali_fgain[0];
    p->cali_fgain1 = c->cali_fgain[1];
    p->cali_fgain2 = c->cali_fgain[2];
    p->cali_fgain3 = c->cali_fgain[3];
    p->cali_comb_fgain0 = c->cali_comb_fgain[0];
    p->cali_comb_fgain1 = c->cali_comb_fgain[1];
    p->cali_comb_fgain2 = c->cali_comb_fgain[2];
    p->cali_comb_fgain3 = c->cali_comb_fgain[3];
    p->map_default = c->map_default;
    /* C drivers point map_unit at static strings */
    p->map_unit = c->map_unit ? g_intern_string(c->map_unit) : NULL;
    p->map_min = c->map_min;
    p->map_max = c->map_max;
}

static void get_device(struct sr_dev_inst *sdi)
{
    struct rs_dev *rs = sdi->priv;
    struct dsv_dev_info info;
    struct dsv_channel c;
    uint64_t gen;
    uint32_t n, i;
    GSList *l;

    dsv_drv_info(rs->h, &info);
    sdi->mode = info.mode;
    sdi->status = info.status;
    set_str(&sdi->path, info.path);

    gen = dsv_drv_channel_generation(rs->h);
    n = dsv_drv_channel_count(rs->h);
    if (gen == rs->generation && g_slist_length(sdi->channels) != n) {
        /* dsl_adjust_probes(): the list grew or shrank at its end, the
         * existing channels stay */
        while (g_slist_length(sdi->channels) > n) {
            GSList *last = g_slist_last(sdi->channels);
            struct sr_channel *p = last->data;
            sdi->channels = g_slist_delete_link(sdi->channels, last);
            safe_free(p->name);
            safe_free(p->trigger);
            g_free(p);
        }
        for (i = g_slist_length(sdi->channels); i < n; i++) {
            struct sr_channel *p;
            if (dsv_drv_channel_get(rs->h, i, &c) != SR_OK)
                break;
            p = sr_channel_new(c.index, c.ty, c.enabled, c.name);
            if (p == NULL)
                break;
            sdi->channels = g_slist_append(sdi->channels, p);
        }
    }
    if (gen != rs->generation || g_slist_length(sdi->channels) != n) {
        sr_dev_probes_free(sdi);
        for (i = 0; i < n; i++) {
            struct sr_channel *p;
            if (dsv_drv_channel_get(rs->h, i, &c) != SR_OK)
                break;
            p = sr_channel_new(c.index, c.ty, c.enabled, c.name);
            if (p == NULL)
                break;
            fill_channel(p, &c);
            sdi->channels = g_slist_append(sdi->channels, p);
        }
        rs->generation = gen;
        return;
    }
    for (i = 0, l = sdi->channels; l; l = l->next, i++) {
        if (dsv_drv_channel_get(rs->h, i, &c) == SR_OK)
            fill_channel(l->data, &c);
    }
}

static int channel_pos(const struct sr_dev_inst *sdi, const struct sr_channel *ch)
{
    int i = 0;
    if (ch == NULL)
        return -1;
    for (GSList *l = sdi->channels; l; l = l->next, i++) {
        if (l->data == ch)
            return i;
    }
    return -1;
}

/* ------------------------------------------------------------ GVariants */

/* sr_list_item arrays handed out by pointer, one per key, kept for the
 * life of the process like the C drivers' static arrays */
#define RS_LIST_SLOTS 16
#define RS_LIST_ITEMS 32
static struct {
    int key;
    struct sr_list_item items[RS_LIST_ITEMS + 1];
} rs_lists[RS_LIST_SLOTS];
static GMutex rs_lists_lock;

static struct sr_list_item *list_slot(int key)
{
    int i;
    for (i = 0; i < RS_LIST_SLOTS; i++) {
        if (rs_lists[i].key == key || rs_lists[i].key == 0) {
            rs_lists[i].key = key;
            return rs_lists[i].items;
        }
    }
    return NULL;
}

static GVariant *to_variant(int key, const struct dsv_conf *v)
{
    GVariantBuilder gvb;
    uint32_t i;

    switch (v->kind) {
    case 1: return g_variant_new_boolean(v->i != 0);
    case 2: return g_variant_new_byte((guint8)v->i);
    case 3: return g_variant_new_int16((gint16)v->i);
    case 4: return g_variant_new_uint16((guint16)v->i);
    case 5: return g_variant_new_int32((gint32)v->i);
    case 6: return g_variant_new_uint32((guint32)v->i);
    case 7: return g_variant_new_int64(v->i);
    case 8: return g_variant_new_uint64(v->u);
    case 9: return g_variant_new_double(v->d);
    case 10: return g_variant_new_string(v->s);
    case 11: return g_variant_new_strv((const gchar *const *)v->strs, v->n);
    case 12:
        return g_variant_new_fixed_array(G_VARIANT_TYPE_INT32, v->arr, v->n, sizeof(int32_t));
    case 13:
        return g_variant_new_fixed_array(G_VARIANT_TYPE_UINT64, v->arr, v->n, sizeof(uint64_t));
    case 14:
        return g_variant_new_fixed_array(G_VARIANT_TYPE_BYTE, v->arr, v->n, sizeof(uint8_t));
    case 15:
        g_variant_builder_init(&gvb, G_VARIANT_TYPE("a{sv}"));
        for (i = 0; i < v->n; i++)
            g_variant_builder_add(&gvb, "{sv}", v->strs[i], to_variant(key, &v->items[i]));
        return g_variant_builder_end(&gvb);
    case 16: {
        struct sr_list_item *items;
        const int32_t *ids = v->arr;
        g_mutex_lock(&rs_lists_lock);
        items = list_slot(key);
        if (items == NULL) {
            g_mutex_unlock(&rs_lists_lock);
            return NULL;
        }
        for (i = 0; i < v->n && i < RS_LIST_ITEMS; i++) {
            items[i].id = ids[i];
            items[i].name = g_intern_string(v->strs[i]);
        }
        items[i].id = -1;
        items[i].name = NULL;
        g_mutex_unlock(&rs_lists_lock);
        return g_variant_new_uint64((uint64_t)(uintptr_t)items);
    }
    }
    return NULL;
}

/* config_set input; strings point into the GVariant */
static void from_variant(GVariant *data, struct dsv_conf *v)
{
    const GVariantType *t = g_variant_get_type(data);

    memset(v, 0, sizeof(*v));
    if (g_variant_type_equal(t, G_VARIANT_TYPE_BOOLEAN)) {
        v->kind = 1; v->i = g_variant_get_boolean(data);
    } else if (g_variant_type_equal(t, G_VARIANT_TYPE_BYTE)) {
        v->kind = 2; v->i = g_variant_get_byte(data);
    } else if (g_variant_type_equal(t, G_VARIANT_TYPE_INT16)) {
        v->kind = 3; v->i = g_variant_get_int16(data);
    } else if (g_variant_type_equal(t, G_VARIANT_TYPE_UINT16)) {
        v->kind = 4; v->i = g_variant_get_uint16(data);
    } else if (g_variant_type_equal(t, G_VARIANT_TYPE_INT32)) {
        v->kind = 5; v->i = g_variant_get_int32(data);
    } else if (g_variant_type_equal(t, G_VARIANT_TYPE_UINT32)) {
        v->kind = 6; v->i = g_variant_get_uint32(data);
    } else if (g_variant_type_equal(t, G_VARIANT_TYPE_INT64)) {
        v->kind = 7; v->i = g_variant_get_int64(data);
    } else if (g_variant_type_equal(t, G_VARIANT_TYPE_UINT64)) {
        v->kind = 8; v->u = g_variant_get_uint64(data);
    } else if (g_variant_type_equal(t, G_VARIANT_TYPE_DOUBLE)) {
        v->kind = 9; v->d = g_variant_get_double(data);
    } else if (g_variant_type_equal(t, G_VARIANT_TYPE_STRING)) {
        v->kind = 10; v->s = g_variant_get_string(data, NULL);
    }
}

/* --------------------------------------------------------- the vtable */

static int rs_config_get(int id, GVariant **data, const struct sr_dev_inst *sdi,
                         const struct sr_channel *ch, const struct sr_channel_group *cg)
{
    struct dsv_conf *v = NULL;
    int ret;
    (void)cg;

    if (sdi == NULL)
        return SR_ERR;
    put_channels(sdi);
    ret = dsv_drv_config_get(H(sdi), id, channel_pos(sdi, ch), &v);
    if (ret == SR_OK && v != NULL)
        *data = to_variant(id, v);
    dsv_conf_free(v);
    return ret;
}

static int rs_config_set(int id, GVariant *data, struct sr_dev_inst *sdi,
                         struct sr_channel *ch, struct sr_channel_group *cg)
{
    struct dsv_conf v;
    int ret;
    (void)cg;

    if (sdi == NULL)
        return SR_ERR;
    from_variant(data, &v);
    put_channels(sdi);
    ret = dsv_drv_config_set(H(sdi), id, channel_pos(sdi, ch), &v);
    get_device(sdi);
    return ret;
}

static int rs_config_list(int key, GVariant **data, const struct sr_dev_inst *sdi,
                          const struct sr_channel_group *cg)
{
    struct dsv_conf *v = NULL;
    int ret;
    (void)cg;

    if (sdi == NULL)
        return SR_ERR_ARG;
    put_channels(sdi);
    ret = dsv_drv_config_list(H(sdi), key, &v);
    if (ret == SR_OK && v != NULL)
        *data = to_variant(key, v);
    dsv_conf_free(v);
    return ret;
}

static const GSList *rs_dev_mode_list(const struct sr_dev_inst *sdi)
{
    struct rs_dev *rs;
    int32_t modes[8];
    uint32_t n, i, k;

    if (sdi == NULL)
        return NULL;
    rs = sdi->priv;
    if (rs->modes == NULL) {
        n = dsv_drv_modes(rs->h, modes, G_N_ELEMENTS(modes));
        for (i = 0; i < n; i++) {
            for (k = 0; k < G_N_ELEMENTS(sr_mode_list); k++) {
                if (sr_mode_list[k].mode == modes[i])
                    rs->modes = g_slist_append(rs->modes, (gpointer)&sr_mode_list[k]);
            }
        }
    }
    return rs->modes;
}

static int rs_dev_open(struct sr_dev_inst *sdi)
{
    int ret, err;
    dsv_drv_set_user_dir(DS_USR_PATH);
    dsv_drv_set_res_dir(DS_RES_PATH);
    if (sdi->dev_type == DEV_TYPE_USB && sdi->conn != NULL) {
        /* lib_main updates bus/address when the device reconnected */
        struct sr_usb_dev_inst *usb = sdi->conn;
        dsv_drv_set_location(H(sdi), usb->bus, usb->address);
    }
    put_channels(sdi);
    ret = dsv_drv_open(H(sdi));
    get_device(sdi);
    if (ret != SR_OK && (err = dsv_drv_last_error(H(sdi))) != 0)
        ds_set_last_error(err);
    return ret;
}

static int rs_dev_close(struct sr_dev_inst *sdi)
{
    int ret;
    if (sdi == NULL || sdi->priv == NULL)
        return SR_ERR_CALL_STATUS;
    put_channels(sdi);
    ret = dsv_drv_close(H(sdi));
    get_device(sdi);
    return ret;
}

static int rs_dev_destroy(struct sr_dev_inst *sdi)
{
    struct rs_dev *rs = sdi->priv;
    rs_dev_close(sdi);
    dsv_drv_free(rs->h);
    g_slist_free(rs->modes);
    sr_dev_inst_free(sdi);
    return SR_OK;
}

static int rs_dev_status_get(const struct sr_dev_inst *sdi, struct sr_status *status, gboolean prg)
{
    if (sdi == NULL)
        return SR_ERR;
    return dsv_drv_status(H(sdi), prg, status, sizeof(*status));
}

static void rs_emit(void *ctx, const struct dsv_packet *p)
{
    const struct sr_dev_inst *sdi = ctx;
    struct sr_datafeed_packet packet;
    struct sr_datafeed_logic logic;
    struct sr_datafeed_dso dso;
    struct sr_datafeed_analog analog;
    struct sr_datafeed_header header;
    struct ds_trigger_pos trigger;

    memset(&packet, 0, sizeof(packet));
    packet.type = p->ty;
    packet.status = p->status;
    switch (p->ty) {
    case SR_DF_HEADER:
        memset(&header, 0, sizeof(header));
        header.feed_version = p->feed_version;
        header.starttime.tv_sec = p->start_sec;
        header.starttime.tv_usec = p->start_usec;
        packet.payload = &header;
        break;
    case SR_DF_TRIGGER:
        trigger.check_id = p->trig_check_id;
        trigger.real_pos = p->trig_real_pos;
        trigger.ram_saddr = p->trig_ram_saddr;
        trigger.remain_cnt_l = p->trig_remain_cnt_l;
        trigger.remain_cnt_h = p->trig_remain_cnt_h;
        trigger.status = p->trig_status;
        packet.payload = &trigger;
        break;
    case SR_DF_LOGIC:
        memset(&logic, 0, sizeof(logic));
        logic.length = p->len;
        logic.format = p->format;
        logic.index = p->index;
        logic.order = p->order;
        logic.unitsize = p->unitsize;
        logic.data_error = p->data_error;
        logic.error_pattern = p->error_pattern;
        logic.data = (void *)p->data;
        packet.payload = &logic;
        break;
    case SR_DF_DSO:
        memset(&dso, 0, sizeof(dso));
        dso.probes = sdi->channels;
        dso.num_samples = p->num_samples;
        dso.mq = p->mq;
        dso.unit = p->unit;
        dso.mqflags = p->mqflags;
        dso.samplerate_tog = p->samplerate_tog;
        dso.trig_flag = p->trig_flag;
        dso.trig_ch = p->trig_ch;
        dso.data = (void *)p->data;
        packet.payload = &dso;
        break;
    case SR_DF_ANALOG:
        memset(&analog, 0, sizeof(analog));
        analog.probes = sdi->channels;
        analog.num_samples = p->num_samples;
        analog.unit_bits = p->unit_bits;
        analog.unit_pitch = p->unit_pitch;
        analog.mq = p->mq;
        analog.unit = p->unit;
        analog.mqflags = p->mqflags;
        analog.data = (void *)p->data;
        packet.payload = &analog;
        break;
    }
    ds_data_forward(sdi, &packet);
}

static int rs_receive(int fd, int revents, const struct sr_dev_inst *sdi)
{
    (void)fd;
    put_channels(sdi);
    if (!dsv_drv_poll(H(sdi), revents == -1, rs_emit, (void *)sdi))
        sr_session_source_remove(-1);
    return TRUE;
}

static int rs_dev_acquisition_start(struct sr_dev_inst *sdi, void *cb_data)
{
    int ret;
    (void)cb_data;

    put_channels(sdi);
    if (trigger != NULL)
        dsv_drv_set_trigger(H(sdi), trigger, sizeof(*trigger));
    ret = dsv_drv_start(H(sdi), rs_emit, sdi);
    get_device(sdi);
    if (ret != SR_OK) {
        int err = dsv_drv_last_error(H(sdi));
        if (err != 0)
            ds_set_last_error(err);
        return ret;
    }
    return sr_session_source_add(-1, 0, 0, rs_receive, sdi);
}

static int rs_dev_acquisition_stop(const struct sr_dev_inst *sdi, void *cb_data)
{
    (void)cb_data;
    return dsv_drv_stop(H(sdi), rs_emit, (void *)sdi);
}

/* ------------------------------------------------------------- demo */

SR_PRIV struct sr_dev_driver rs_demo_driver_info;

static int rs_demo_init(struct sr_context *sr_ctx)
{
    dsv_drv_set_log(rs_log);
    return std_hw_init(sr_ctx, &rs_demo_driver_info, LOG_PREFIX);
}

static int rs_cleanup(void)
{
    return SR_OK;
}

static GSList *rs_demo_scan(GSList *options)
{
    struct dsv_device *handles[8];
    GSList *devices = NULL;
    uint32_t n, i;
    (void)options;

    n = dsv_drv_demo_scan(handles, G_N_ELEMENTS(handles));
    for (i = 0; i < n; i++) {
        struct dsv_dev_info info;
        struct sr_dev_inst *sdi;
        struct rs_dev *rs;

        dsv_drv_info(handles[i], &info);
        sdi = sr_dev_inst_new(info.mode, info.status, info.vendor, info.model, info.version);
        rs = g_try_malloc0(sizeof(*rs));
        if (sdi == NULL || rs == NULL) {
            sr_err("Device instance creation failed.");
            g_free(rs);
            sr_dev_inst_free(sdi);
            dsv_drv_free(handles[i]);
            continue;
        }
        rs->h = handles[i];
        rs->generation = (uint64_t)-1;
        sdi->priv = rs;
        sdi->driver = &rs_demo_driver_info;
        sdi->dev_type = info.dev_type;
        get_device(sdi);
        devices = g_slist_append(devices, sdi);
    }
    return devices;
}

SR_PRIV struct sr_dev_driver rs_demo_driver_info = {
    .name = "virtual-demo",
    .longname = "Demo driver and pattern generator (Rust)",
    .api_version = 1,
    .driver_type = DRIVER_TYPE_DEMO,
    .init = rs_demo_init,
    .cleanup = rs_cleanup,
    .scan = rs_demo_scan,
    .dev_mode_list = rs_dev_mode_list,
    .config_get = rs_config_get,
    .config_set = rs_config_set,
    .config_list = rs_config_list,
    .dev_open = rs_dev_open,
    .dev_close = rs_dev_close,
    .dev_destroy = rs_dev_destroy,
    .dev_status_get = rs_dev_status_get,
    .dev_acquisition_start = rs_dev_acquisition_start,
    .dev_acquisition_stop = rs_dev_acquisition_stop,
    .priv = NULL,
};

/* ---------------------------------------------------------- DSLogic */

SR_PRIV struct sr_dev_driver rs_dslogic_driver_info;

static int rs_dslogic_init(struct sr_context *sr_ctx)
{
    dsv_drv_set_log(rs_log);
    return std_hw_init(sr_ctx, &rs_dslogic_driver_info, LOG_PREFIX);
}

/* The libusb device lib_main's hotplug handling knows this device as. */
static libusb_device *find_libusb_device(uint8_t bus, uint8_t address)
{
    struct drv_context *drvc = rs_dslogic_driver_info.priv;
    libusb_device **list = NULL, *found = NULL;

    if (drvc == NULL || libusb_get_device_list(drvc->sr_ctx->libusb_ctx, &list) < 0 || list == NULL)
        return NULL;
    for (int i = 0; list[i]; i++) {
        if (libusb_get_bus_number(list[i]) == bus && libusb_get_device_address(list[i]) == address) {
            found = libusb_ref_device(list[i]);
            break;
        }
    }
    libusb_free_device_list(list, 1);
    return found;
}

/* scan(): the Rust driver finds and probes the devices; each one is keyed
 * by lib_main's libusb_device, so hotplug and reconnect work as for C. */
static GSList *rs_dslogic_scan(GSList *options)
{
    struct dsv_usb_location locs[16];
    GSList *devices = NULL;
    int32_t slow = 0;
    uint32_t n, i;
    int num = 0;
    (void)options;

    dsv_drv_set_res_dir(DS_RES_PATH);
    sr_info("Scan DSLogic device...");
    n = dsv_drv_dslogic_scan(locs, G_N_ELEMENTS(locs), &slow);
    for (i = 0; i < n; i++) {
        struct dsv_device *h = NULL;
        struct dsv_dev_info info;
        struct sr_dev_inst *sdi;
        struct sr_usb_dev_inst *usb;
        struct rs_dev *rs;
        libusb_device *dev = find_libusb_device(locs[i].bus, locs[i].address);

        if (dev == NULL)
            continue;
        if (sr_usb_device_is_exists(dev)) {
            sr_detail("Device is exists, handle: %p", dev);
            libusb_unref_device(dev);
            continue;
        }
        sr_info("Found a new device,handle:%p", dev);
        if (dsv_drv_dslogic_probe(locs[i].bus, locs[i].address, &h) != 1) {
            /* firmware uploaded (it re-enumerates) or not answering */
            libusb_unref_device(dev);
            continue;
        }
        dsv_drv_info(h, &info);
        sdi = sr_dev_inst_new(info.mode, SR_ST_INITIALIZING, info.vendor, info.model, info.version);
        rs = g_try_malloc0(sizeof(*rs));
        usb = sr_usb_dev_inst_new(locs[i].bus, locs[i].address);
        if (sdi == NULL || rs == NULL || usb == NULL) {
            g_free(rs);
            g_free(usb);
            sr_dev_inst_free(sdi);
            dsv_drv_free(h);
            libusb_unref_device(dev);
            continue;
        }
        rs->h = h;
        rs->generation = (uint64_t)-1;
        sdi->priv = rs;
        sdi->driver = &rs_dslogic_driver_info;
        sdi->dev_type = DEV_TYPE_USB;
        sdi->handle = (ds_device_handle)dev;
        usb->usb_dev = dev;
        sdi->conn = usb;
        get_device(sdi);
        sdi->status = SR_ST_INACTIVE;
        dsv_drv_set_status(h, SR_ST_INACTIVE);
        devices = g_slist_append(devices, sdi);
        num++;
    }
    sr_info("Found new DSLogic device count: %d", num);
    if (slow)
        post_message_callback(DS_EV_DEVICE_SPEED_NOT_MATCH);
    return devices;
}

SR_PRIV struct sr_dev_driver rs_dslogic_driver_info = {
    .name = "DSLogic",
    .longname = "DSLogic (generic driver, Rust)",
    .api_version = 1,
    .driver_type = DRIVER_TYPE_HARDWARE,
    .init = rs_dslogic_init,
    .cleanup = rs_cleanup,
    .scan = rs_dslogic_scan,
    .dev_mode_list = rs_dev_mode_list,
    .config_get = rs_config_get,
    .config_set = rs_config_set,
    .config_list = rs_config_list,
    .dev_open = rs_dev_open,
    .dev_close = rs_dev_close,
    .dev_destroy = rs_dev_destroy,
    .dev_status_get = rs_dev_status_get,
    .dev_acquisition_start = rs_dev_acquisition_start,
    .dev_acquisition_stop = rs_dev_acquisition_stop,
    .priv = NULL,
};

/* ------------------------------------------------------------- file */

SR_PRIV struct sr_dev_driver rs_file_driver_info;

static int rs_file_init(struct sr_context *sr_ctx)
{
    (void)sr_ctx;
    dsv_drv_set_log(rs_log);
    return SR_OK;
}

/* sr_new_virtual_device() with DSVIEW_IMPL_DRV_FILE=rust */
SR_PRIV int rs_new_virtual_device(const char *filename, struct sr_dev_inst **out_di)
{
    struct dsv_device *h = NULL;
    struct dsv_dev_info info;
    struct sr_dev_inst *sdi;
    struct rs_dev *rs;
    int ret;

    if (!filename) {
        sr_err("%s: filename was NULL", __func__);
        return SR_ERR_ARG;
    }
    if (out_di == NULL) {
        sr_err("%s: @out_di was NULL", __func__);
        return SR_ERR_ARG;
    }
    dsv_drv_set_log(rs_log);
    ret = dsv_drv_file_new(filename, &h);
    if (ret != SR_OK)
        return ret;

    dsv_drv_info(h, &info);
    sdi = sr_dev_inst_new(info.mode, info.status, NULL, NULL, NULL);
    rs = g_try_malloc0(sizeof(*rs));
    if (sdi == NULL || rs == NULL) {
        g_free(rs);
        sr_dev_inst_free(sdi);
        dsv_drv_free(h);
        return SR_ERR_MALLOC;
    }
    rs->h = h;
    rs->generation = (uint64_t)-1;
    sdi->priv = rs;
    sdi->driver = &rs_file_driver_info;
    sdi->dev_type = DEV_TYPE_FILELOG;
    sdi->name = g_strdup(info.model);
    get_device(sdi);
    *out_di = sdi;
    return SR_OK;
}

SR_PRIV struct sr_dev_driver rs_file_driver_info = {
    .name = "virtual-session",
    .longname = "Session-emulating driver (Rust)",
    .api_version = 1,
    .driver_type = DRIVER_TYPE_FILE,
    .init = rs_file_init,
    .cleanup = rs_cleanup,
    .scan = NULL,
    .dev_mode_list = rs_dev_mode_list,
    .config_get = rs_config_get,
    .config_set = rs_config_set,
    .config_list = rs_config_list,
    .dev_open = rs_dev_open,
    .dev_close = rs_dev_close,
    .dev_destroy = rs_dev_destroy,
    .dev_status_get = rs_dev_status_get,
    .dev_acquisition_start = rs_dev_acquisition_start,
    /* the C session driver has none */
    .dev_acquisition_stop = NULL,
    .priv = NULL,
};

#endif /* DSVIEW_RUST_CORE */

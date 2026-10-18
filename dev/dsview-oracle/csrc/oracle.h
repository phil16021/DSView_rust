/* Interface between the Rust oracle binaries and the original C libraries. */

#ifndef DSVIEW_ORACLE_H
#define DSVIEW_ORACLE_H

#include <stdint.h>

/* ------------------------------------------------------------ decode */

struct oracle_opt {
    const char *key;
    const char *str;   /* JSON string value, or "" */
    double num;        /* JSON number value */
    int is_num;        /* 1 when the JSON value was a number */
};

struct oracle_dec {
    const char *id;
    const struct oracle_opt *opts;
    int n_opts;
    const char *const *ch_id;  /* decoder channel ids ... */
    const int *ch_idx;         /* ... bound to these signal indexes */
    int n_ch;
};

typedef void (*oracle_emit)(void *user, const char *json_line);

int oracle_decode_init(const char *decoder_dir, int log_level);
void oracle_decode_exit(void);

/* signals[i]: bit-packed LSB-first samples of signal i (NULL if absent),
 * laid out like LogicSnapshot leaves (2^24 samples per leaf from sample 0).
 * max_chunk 0 = DecoderStack::MaxChunkSize. */
/* Every loaded decoder's metadata, one JSON line each (see oracle_decode.c). */
void oracle_decode_list(oracle_emit emit, void *user);

int oracle_decode_run(const struct oracle_dec *stack, int n_dec, uint64_t samplerate,
                      uint64_t total_samples, const uint8_t *const *signals, int n_signals,
                      uint64_t max_chunk, oracle_emit emit, void *user);

/* ------------------------------------------------------------ .dsl loading */

#define ORACLE_MAX_CHANNELS 64

struct oracle_channel {
    int index;
    int type;      /* enum oracle_channel_kind */
    int enabled;
    char name[64];
};

struct oracle_dsl_info {
    int mode;      /* LOGIC=0, DSO=1, ANALOG=2 */
    uint64_t samplerate;
    uint64_t limit_samples;
    int n_channels;
    struct oracle_channel channels[ORACLE_MAX_CHANNELS];
};

/* Packet kinds forwarded from the libsigrok datafeed (mapped from SR_DF_*
 * on the C side so the Rust side never mirrors enum values). */
enum oracle_packet_kind {
    ORACLE_PKT_OTHER = 0,
    ORACLE_PKT_LOGIC = 1,     /* format: 0 = LA_CROSS_DATA */
    ORACLE_PKT_DSO = 2,
    ORACLE_PKT_ANALOG = 3,
    ORACLE_PKT_END = 4,
};

/* Channel kinds (mapped from SR_CHANNEL_*). */
enum oracle_channel_kind {
    ORACLE_CH_OTHER = 0,
    ORACLE_CH_LOGIC = 1,
    ORACLE_CH_DSO = 2,
    ORACLE_CH_ANALOG = 3,
};

typedef void (*oracle_packet_cb)(void *user, int type, int format, const uint8_t *data,
                                 uint64_t len, int num_samples);

/* Loads a .dsl/.demo through libsigrok4DSL's session driver (the code path
 * DSView uses for File > Open) and forwards every datafeed packet. */
int oracle_dsl_run(const char *path, int log_level, struct oracle_dsl_info *info,
                   oracle_packet_cb cb, void *user);

/* ------------------------------------------------------------ export modules */

struct oracle_export_channel {
    int index;
    int kind;              /* enum oracle_channel_kind */
    int enabled;
    const char *name;
    uint64_t vdiv;
    uint64_t vfactor;
    uint16_t hw_offset;
    double map_max;
    double map_min;
    const char *map_unit;
};

enum oracle_export_type { ORACLE_EXP_META, ORACLE_EXP_LOGIC, ORACLE_EXP_DSO, ORACLE_EXP_ANALOG, ORACLE_EXP_END };

struct oracle_export_packet {
    int type;              /* enum oracle_export_type */
    uint64_t samplerate;
    uint64_t limit_samples;
    int has_ref;
    uint32_t ref_min;
    uint32_t ref_max;
    const uint8_t *data;
    uint64_t len;
    uint16_t unitsize;
    int num_samples;
    int export_original;
};

/* Runs libsigrok4DSL output module `module_id` (csv, vcd, ...) for a capture
 * of channel kind `kind` and returns all text it produced (free with
 * oracle_free). Returns 0, or -1 when the module rejects the input. */
int oracle_export_run(const char *module_id, int kind, uint64_t start_index, const char *time_string,
                      const struct oracle_export_channel *chans, int n_ch,
                      const struct oracle_export_packet *pkts, int n_pkts, char **out);
void oracle_free(void *p);

/* ------------------------------------------------------------ LogicSnapshot */

/* The original pv::data::LogicSnapshot (csrc/oracle_store.cpp). Signal
 * indexes are DSView channel indexes; sample data is LA_CROSS. */
typedef struct oracle_store oracle_store;

struct ostore_counts {
    uint64_t sample_count;
    uint64_t ring_sample_count;
    uint64_t total_sample_count;
    uint64_t loop_offset;
    uint64_t ring_start;
    uint64_t ring_end;
    int block_num;
    int last_ended;
    unsigned int channel_num;
};

oracle_store *ostore_new(void);
void ostore_free(oracle_store *s);
void ostore_set_loop(oracle_store *s, int loop);
void ostore_first_payload(oracle_store *s, const uint8_t *data, uint64_t len, uint64_t total,
                          const uint16_t *ch, int n_ch, int able_free);
void ostore_append(oracle_store *s, const uint8_t *data, uint64_t len);
void ostore_capture_ended(oracle_store *s);
void ostore_clear(oracle_store *s);
void ostore_counts(oracle_store *s, struct ostore_counts *c);
uint64_t ostore_block_size(oracle_store *s, int block);
const uint8_t *ostore_block_buf(oracle_store *s, int block, int sig, int *sample);
const uint8_t *ostore_get_samples(oracle_store *s, uint64_t start, uint64_t *end, int sig, void **lbp);
int ostore_get_sample(oracle_store *s, uint64_t index, int sig);
int ostore_has_data(oracle_store *s, int sig);
/* edges[i] = (is_edge << 1) | level; buffers hold `width` edges and
 * `max_togs` + 1 toggles. Returns the first sample's level. */
int ostore_display_edges(oracle_store *s, uint64_t start, uint64_t end, uint16_t width,
                         uint16_t max_togs, double pixels_offset, double min_length, uint16_t sig,
                         uint8_t *edges, int *n_edges, uint16_t *tog_pos, uint8_t *tog_val,
                         int *n_togs);
int ostore_nxt_edge(oracle_store *s, uint64_t *index, int last_sample, uint64_t end,
                    double min_length, int sig);
int ostore_pre_edge(oracle_store *s, uint64_t *index, int last_sample, double min_length, int sig);
/* flags[i] is one of 0 1 X R F C for channel chs[i]. */
int ostore_pattern_search(oracle_store *s, int64_t start, int64_t end, int64_t *index,
                          const uint16_t *chs, const char *flags, int n, int is_next);
int ostore_block_index_with_sample(oracle_store *s, uint64_t sample, uint64_t *offset);
void ostore_decode_end(oracle_store *s);
void ostore_free_decode_lpb(oracle_store *s, void *lbp);

/* Internal state, for comparing the stores exactly. dest_*: the leaf and
 * byte the unfinished word is written to (-1 none, dest_byte -2 dangling). */
struct ostore_internals {
    uint8_t byte_fraction;
    uint16_t ch_fraction;
    int64_t dest_order, dest_root, dest_pos, dest_byte;
    uint64_t last_sample[64];
    uint64_t last_calc_count[64];
    uint64_t cur_ref_root[64];
    uint64_t cur_ref_lbp[64];
    int is_loop;
    int able_free;
    uint64_t kept_leaves;
    int lst_free_block_index;
    uint64_t n_orders;
};
void ostore_internals(oracle_store *s, struct ostore_internals *o);
int ostore_roots(oracle_store *s, int order);
/* bits: tog, first, last; leaves: 64 leaf pointers (NULL = freed). A leaf
 * is 266305 words: samples, then mip-map levels 1-3. */
void ostore_root(oracle_store *s, int order, int root, uint64_t *bits, const uint64_t **leaves);

/* ------------------------------------------------------------ Dso/AnalogSnapshot */

/* The original pv::data::DsoSnapshot and AnalogSnapshot
 * (csrc/oracle_store_ad.cpp). Channel lists: index and enabled flag of the
 * device's DSO (analog) channels. *_zero clears the malloc'ed buffers, which
 * C++ leaves uninitialised; call it after a first_payload without samples. */
typedef struct osdso osdso;
typedef struct osana osana;

struct osdso_counts {
    uint64_t sample_count, total_sample_count, ring_sample_count;
    unsigned int channel_num;
    int last_ended, memory_failed, is_file, instant, out_of_range, block_num;
    uint64_t n_orders;
};

struct osana_counts {
    uint64_t sample_count, total_sample_count, ring_sample_count, capacity;
    unsigned int channel_num;
    uint8_t unit_bytes;
    uint16_t unit_pitch;
    int last_ended, memory_failed, block_num;
};

struct osana_section {
    uint64_t start;
    unsigned int scale;
    uint64_t length, samples_num;
    const uint8_t *samples; /* min, max byte pairs */
};

osdso *osdso_new(void);
void osdso_free(osdso *s);
void osdso_clear(osdso *s);
void osdso_init(osdso *s);
void osdso_capture_ended(osdso *s);
void osdso_set_ref_range(osdso *s, uint32_t ref_max, uint32_t ref_min);
void osdso_zero(osdso *s);
void osdso_counts(osdso *s, struct osdso_counts *c);
uint64_t osdso_block_size(osdso *s, int block);
int osdso_has_data(osdso *s, int sig);
const uint8_t *osdso_buffer(osdso *s, int order);
const uint8_t *osdso_get_samples(osdso *s, uint64_t start, int sig);
int osdso_max_min(osdso *s, int order, uint8_t *maxv, uint8_t *minv);
void osdso_first_payload(osdso *s, const uint8_t *data, int num_samples, uint64_t total,
                         const uint16_t *idx, const int *enabled, int n, int instant, int is_file,
                         int samplerate_tog);
void osdso_append(osdso *s, const uint8_t *data, int num_samples, int samplerate_tog);
void osdso_enable_envelope(osdso *s, int enable);
void osdso_envelope_section(osdso *s, uint64_t start, uint64_t end, float min_length, int probe,
                            uint64_t *out_start, unsigned *out_scale, uint64_t *out_length,
                            const uint8_t **out_samples);

osana *osana_new(void);
void osana_free(osana *s);
void osana_clear(osana *s);
void osana_init(osana *s);
void osana_capture_ended(osana *s);
void osana_zero(osana *s);
void osana_counts(osana *s, struct osana_counts *c);
uint64_t osana_block_size(osana *s, int block);
int osana_has_data(osana *s, int sig);
int osana_has_enabled_channel(osana *s, int sig);
int osana_ch_order(osana *s, int sig);
const uint8_t *osana_data(osana *s);
/* Bytes of allocator slack after an envelope ring (see osana_zero). */
int osana_envelope_slack(osana *s, int probe, int level);
void osana_envelope(osana *s, int probe, int level, uint64_t *len, uint64_t *ring_len, uint64_t *count,
                    const uint8_t **samples);
void osana_section(osana *s, uint64_t start, int64_t count, float min_length, int probe,
                   struct osana_section *out);
void osana_first_payload(osana *s, const uint8_t *data, int num_samples, uint8_t unit_bits,
                         uint16_t unit_pitch, uint64_t total, const uint16_t *idx, const int *enabled, int n);
void osana_append(osana *s, const uint8_t *data, int num_samples, uint16_t unit_pitch);

/* ------------------------------------------------------------ drivers */

/* A datafeed packet as the GUI's callback sees it. */
struct oracle_drv_packet {
    int type;              /* SR_DF_* */
    int status;            /* SR_PKT_* */
    const void *data;
    uint64_t len;          /* payload bytes the GUI reads */
    int num_samples;
    int format, index, order, unitsize, data_error;
    uint64_t error_pattern;
    int mq, unit;
    uint64_t mqflags;
    int samplerate_tog, trig_flag, trig_ch;
    int unit_bits, unit_pitch;
    int nprobes;
    int feed_version;          /* SR_DF_HEADER (the start time is wall clock) */
    uint32_t trig_real_pos;    /* SR_DF_TRIGGER */
};

typedef void (*oracle_drv_packet_cb)(void *user, const struct oracle_drv_packet *p);

int oracle_drv_init(const char *user_dir, int log_level);
void oracle_drv_exit(void);
int oracle_drv_activate(const char *name);
int oracle_drv_open_file(const char *path);
char *oracle_drv_get(int key, int ch);
char *oracle_drv_list(int key);
int oracle_drv_set(int key, int ch, const char *text);
int oracle_drv_enable(int ch, int enable);
char *oracle_drv_state(void);
int oracle_drv_capture(int stop_after, oracle_drv_packet_cb cb, void *user);

/* The virtual clock (Rust side) behind demo.c's timers. */
int64_t oracle_clock_now_us(void);

#endif

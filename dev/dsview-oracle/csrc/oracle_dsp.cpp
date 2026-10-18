/* The numeric kernels of pv/data/spectrumstack.cpp, for comparing
 * dsview-dsp. SpectrumStack itself needs a SigSession and DsoSignals, so
 * the body of calc_fft() (from "prepare _xn data" on) and window() are
 * copied here verbatim; only the inputs become parameters. Linked with the
 * system FFTW, as DSView is. See oracle.h. */

#include <math.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <algorithm>
#include <vector>
#include <fftw3.h>

#include "libsigrok.h"

extern "C" {
#include "oracle.h"
}

#define PI 3.1415

using namespace std;

static double window(uint64_t i, int type, uint64_t _sample_num)
{
    const double n_m_1 = _sample_num-1;
    switch(type) {
    case 1: // Hann window
        return 0.5*(1-cos(2*PI*i/n_m_1));
    case 2: // Hamming window
        return 0.54-0.46*cos(2*PI*i/n_m_1);
    case 3: // Blackman window
        return 0.42659-0.49656*cos(2*PI*i/n_m_1) + 0.076849*cos(4*PI*i/n_m_1);
    case 4: // Flat_top window
        return 1-1.93*cos(2*PI*i/n_m_1)+1.29*cos(4*PI*i/n_m_1)-
                 0.388*cos(6*PI*i/n_m_1)+0.028*cos(8*PI*i/n_m_1);
    default:
        return 1;
    }
}

int osdsp_power_spectrum(const uint8_t *samples, uint64_t _sample_num, uint16_t step,
                         int offset, uint64_t vdiv, uint64_t factor, int _windows_index,
                         double *out)
{
    if (_sample_num < 2)
        return -1;
    std::vector<double> _xn(_sample_num), _xk(_sample_num), _power_spectrum(_sample_num/2+1);
    fftw_plan _fft_plan = fftw_plan_r2r_1d(_sample_num, _xn.data(), _xk.data(),
                                           FFTW_R2HC, FFTW_ESTIMATE);

    const double vscale = vdiv * factor * DS_CONF_DSO_VDIVS / (1000*255.0);
    double wsum = 0;

    for (unsigned int i = 0; i < _sample_num; i++) {
        double w = window(i, _windows_index, _sample_num);
        _xn[i] = (samples[i*step] - offset) * vscale * w;
        wsum += w;
    }

    // fft
    fftw_execute(_fft_plan);

    // calculate power spectrum
    _power_spectrum[0] = abs(_xk[0])/wsum;  /* DC component */
    for (unsigned int k = 1; k < (_sample_num + 1) / 2; ++k)  /* (k < N/2 rounded up) */
         _power_spectrum[k] = sqrt((_xk[k]*_xk[k] + _xk[_sample_num-k]*_xk[_sample_num-k]) * 2) / wsum;
    if (_sample_num % 2 == 0) /* N is even */
         _power_spectrum[_sample_num/2] = abs(_xk[_sample_num/2])/wsum;  /* Nyquist freq. */

    fftw_destroy_plan(_fft_plan);
    for (uint64_t k = 0; k < _sample_num/2+1; k++)
        out[k] = _power_spectrum[k];
    return 0;
}

/* ---- pv/data/mathstack.cpp ---------------------------------------------
 * calc_math()'s loops, default_vDialValue(), default_factor(), the values of
 * get_vDial() and append_to_envelope_level(true), copied verbatim with the
 * DsoSignal getters turned into parameters. */

enum MathType {
    MATH_ADD,
    MATH_SUB,
    MATH_MUL,
    MATH_DIV,
    MATH_INTEG,
    MATH_DIFF,
    MATH_ABS,
    MATH_SQUARE,
    MATH_SQRT,
    MATH_LOWPASS,
    MATH_HIGHPASS,
};

static inline bool is_unary(int type) { return type >= MATH_INTEG; }

static const int vDialValueCount = 19;
static const uint64_t vDialValue[vDialValueCount] = {
    1, 2, 5, 10, 20, 50, 100, 200, 500, 1000, 2000, 5000, 10000, 20000, 50000,
    100000, 200000, 500000, 1000000,
};

int osdsp_math(int _type, const uint8_t *value_buffer1, const uint8_t *value_buffer2,
               uint64_t _sample_num, double scale1, double delta1, double scale2,
               double delta2, double rate, int _filter_width, uint64_t mathFactor,
               double *_math)
{
    auto samplerate = [&]() { return rate; };
    const bool unary = is_unary(_type);

    // Source #1 in volts for a given sample index.
    auto v1 = [&](uint64_t s) -> double {
        return delta1 - scale1 * (*(value_buffer1 + s));
    };

    if (unary) {
        const double dt = (samplerate() > 0) ? 1.0 / samplerate() : 1.0;
        const uint64_t win = (uint64_t)max(1, _filter_width);
        double integ = 0.0;
        double acc = 0.0;   // running window sum for the filters

        for (uint64_t sample = 0; sample < _sample_num; sample++) {
            const double v = v1(sample);
            double r = 0.0;

            switch(_type)
            {
                case MATH_INTEG:
                    integ += v * dt;
                    r = integ;
                    break;
                case MATH_DIFF:
                    r = (v - v1(sample == 0 ? 0 : sample - 1)) / dt;
                    break;
                case MATH_ABS:
                    r = fabs(v);
                    break;
                case MATH_SQUARE:
                    r = v * v;
                    break;
                case MATH_SQRT:
                    r = (v < 0) ? -sqrt(-v) : sqrt(v);
                    break;
                case MATH_LOWPASS:
                case MATH_HIGHPASS: {
                    acc += v;
                    if (sample >= win)
                        acc -= v1(sample - win);
                    const uint64_t n = min<uint64_t>(sample + 1, win);
                    const double avg = acc / n;
                    r = (_type == MATH_LOWPASS) ? avg : (v - avg);
                    break;
                }
                default:
                    r = v;
                    break;
            }

            _math[sample] = r / mathFactor;
        }
    }
    else {
        for (uint64_t sample = 0; sample < _sample_num; sample++) {
            const double value1 = v1(sample);
            const double value2 = delta2 - scale2 * (*(value_buffer2 + sample));

            switch(_type)
            {
                case MATH_ADD:
                    _math[sample] = (value1 + value2) / mathFactor;
                    break;
                case MATH_SUB:
                    _math[sample] = (value1 - value2) / mathFactor;
                    break;
                case MATH_MUL:
                    _math[sample] = value1 * value2 / mathFactor;
                    break;
                case MATH_DIV:
                    _math[sample] = value1 / value2 / mathFactor;
                    break;
                default:
                    break;
            }
        }
    }
    return 0;
}

void osdsp_math_dial(int _type, uint64_t dial1_value, uint64_t factor1, uint64_t dial1_min,
                     uint64_t dial1_max, uint64_t dial2_value, uint64_t factor2,
                     uint64_t dial2_min, uint64_t dial2_max, uint64_t *out_value,
                     uint64_t *out_factor, uint64_t *values, int *count)
{
    /* default_vDialValue */
    {
        uint64_t value = 0;
        const uint64_t v1 = factor1 * dial1_value;
        const uint64_t v2 = factor2 * dial2_value;

        switch(_type) {
        case MATH_ADD:
        case MATH_SUB:
            value = v1 > v2 ? dial1_value : dial2_value;
            break;
        case MATH_MUL:
            value = dial1_value * dial2_value / 1000.0;
            break;
        case MATH_DIV:
            value = dial1_value * 1000.0 / dial2_value;
            break;
        default:  // unary operators: scale off the single source
            value = dial1_value;
            break;
        }

        bool bFind = false;

        for (int i = 0; i < vDialValueCount; i++) {
            if (vDialValue[i] >= value) {
                value = vDialValue[i];
                bFind = true;
                break;
            }
        }

        if (!bFind){
            value = vDialValue[vDialValueCount-1];
        }
        *out_value = value;
    }
    /* default_factor */
    {
        uint64_t value = 0;
        const uint64_t v1 = dial1_value * factor1;
        const uint64_t v2 = dial2_value * factor2;

        switch(_type) {
            case MATH_ADD:
            case MATH_SUB:
                value = v1 > v2 ? factor1 : factor2;
                break;
            case MATH_MUL:
                value = factor1 * factor2;
                break;
            case MATH_DIV:
                value = factor1 / factor2;
                break;
            default:  // unary operators: single-source factor
                value = factor1;
                break;
        }

        if (value == 0){
            value = 1;
        }
        *out_factor = value;
    }
    /* get_vDial()'s values */
    {
        int n = 0;
        auto append = [&](uint64_t v) { values[n++] = v; };
        switch(_type) {
        case MATH_ADD:
        case MATH_SUB:
            for (int i = 0; i < vDialValueCount; i++) {
                if (vDialValue[i] < min(dial1_min, dial2_min))
                    continue;
                append(vDialValue[i]);
                if (vDialValue[i] > max(dial1_max, dial2_max))
                    break;
            }
            break;
        case MATH_MUL:
            for (int i = 0; i < vDialValueCount; i++) {
                if (vDialValue[i] < dial1_min * dial2_min / 1000.0)
                    continue;
                append(vDialValue[i]);
                if (vDialValue[i] > dial1_max * dial2_max / 1000.0)
                    break;
            }
            break;
        case MATH_DIV:
            for (int i = 0; i < vDialValueCount; i++) {
                if (vDialValue[i] < min(dial1_min * 1000.0 / dial2_max, dial2_min * 1000.0  / dial1_max))
                    continue;
                append(vDialValue[i]);
                if (vDialValue[i] > max(dial1_max * 1000.0 / dial2_min, dial2_max * 1000.0 / dial1_min))
                    break;
            }
            break;
        default:  // unary operators: derive the dial from the single source
            for (int i = 0; i < vDialValueCount; i++) {
                if (vDialValue[i] < dial1_min)
                    continue;
                append(vDialValue[i]);
                if (vDialValue[i] > dial1_max)
                    break;
            }
            break;
        }
        *count = n;
    }
}

struct MathEnvelopeSample { double min; double max; };
struct MathEnvelope { uint64_t length; uint64_t data_length; MathEnvelopeSample *samples; };
static const int EnvelopeScalePower = 8;
static const int EnvelopeScaleFactor = 1 << EnvelopeScalePower;
static const uint64_t EnvelopeDataUnit = 4*1024;
static const unsigned int ScaleStepCount = 10;

static void reallocate_envelope(MathEnvelope &e)
{
    const uint64_t new_data_length = ((e.length + EnvelopeDataUnit - 1) /
        EnvelopeDataUnit) * EnvelopeDataUnit;
    if (new_data_length > e.data_length)
    {
        e.data_length = new_data_length;
    }
}

/* realloc() then append_to_envelope_level(true) on a fresh stack; copies
 * level `level` (up to `cap` entries) to out_min/out_max. */
uint64_t osdsp_math_envelope(const double *math, uint64_t _sample_num, int level,
                             double *out_min, double *out_max, uint64_t cap)
{
    MathEnvelope _envelope_level[ScaleStepCount];
    memset(_envelope_level, 0, sizeof(_envelope_level));
    std::vector<double> _math(math, math + _sample_num);
    /* realloc(num) */
    uint64_t envelop_count = _sample_num / EnvelopeScaleFactor;
    for (unsigned int l = 0; l < ScaleStepCount; l++) {
        envelop_count = ((envelop_count + EnvelopeDataUnit - 1) /
                EnvelopeDataUnit) * EnvelopeDataUnit;
        _envelope_level[l].samples = (MathEnvelopeSample*)malloc(envelop_count * sizeof(MathEnvelopeSample));
        envelop_count = envelop_count / EnvelopeScaleFactor;
    }

    const bool header = true;
    MathEnvelope &e0 = _envelope_level[0];
    uint64_t prev_length;
    MathEnvelopeSample *dest_ptr;

    if (header)
        prev_length = 0;
    else
        prev_length = e0.length;
    e0.length = _sample_num / EnvelopeScaleFactor;

    if (e0.length != 0) {
    if (e0.length == prev_length)
        prev_length = 0;

    // Expand the data buffer to fit the new samples
    reallocate_envelope(e0);

    dest_ptr = e0.samples + prev_length;

    // Iterate through the samples to populate the first level mipmap
    const double *const stop_src_ptr = (double*)_math.data() +
        e0.length * EnvelopeScaleFactor;
    for (const double *src_ptr = (double*)_math.data() +
        prev_length * EnvelopeScaleFactor;
        src_ptr < stop_src_ptr; src_ptr += EnvelopeScaleFactor)
    {
        const double * begin_src_ptr =
            src_ptr;
        const double *const end_src_ptr =
            src_ptr + EnvelopeScaleFactor;

        MathEnvelopeSample sub_sample;
        sub_sample.min = *begin_src_ptr;
        sub_sample.max = *begin_src_ptr;
        //begin_src_ptr += _channel_num;
        while (begin_src_ptr < end_src_ptr)
        {
            sub_sample.min = min(sub_sample.min, *begin_src_ptr);
            sub_sample.max = max(sub_sample.max, *begin_src_ptr);
            begin_src_ptr ++;
        }
        *dest_ptr++ = sub_sample;
    }

    // Compute higher level mipmaps
    for (unsigned int level = 1; level < ScaleStepCount; level++)
    {
        MathEnvelope &e = _envelope_level[level];
        const MathEnvelope &el = _envelope_level[level-1];

        // Expand the data buffer to fit the new samples
        prev_length = e.length;
        e.length = el.length / EnvelopeScaleFactor;

        if (e.length == prev_length)
            prev_length = 0;

        reallocate_envelope(e);

        // Subsample the level lower level
        const MathEnvelopeSample *src_ptr =
            el.samples + prev_length * EnvelopeScaleFactor;
        const MathEnvelopeSample *const end_dest_ptr = e.samples + e.length;
        for (dest_ptr = e.samples + prev_length;
            dest_ptr < end_dest_ptr; dest_ptr++)
        {
            const MathEnvelopeSample *const end_src_ptr =
                src_ptr + EnvelopeScaleFactor;

            MathEnvelopeSample sub_sample = *src_ptr++;
            while (src_ptr < end_src_ptr)
            {
                sub_sample.min = min(sub_sample.min, src_ptr->min);
                sub_sample.max = max(sub_sample.max, src_ptr->max);
                src_ptr++;
            }

            *dest_ptr = sub_sample;
        }
    }
    }

    const MathEnvelope &e = _envelope_level[level];
    uint64_t n = min(e.length, cap);
    for (uint64_t i = 0; i < n; i++) {
        out_min[i] = e.samples[i].min;
        out_max[i] = e.samples[i].max;
    }
    for (auto &l : _envelope_level)
        free(l.samples);
    return e.length;
}

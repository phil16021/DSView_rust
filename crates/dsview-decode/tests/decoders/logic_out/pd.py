import sigrokdecode as srd

class Decoder(srd.Decoder):
    api_version = 3
    id = 'logic_out'
    name = 'Logic out'
    longname = 'Logic output'
    desc = 'Test decoder for OUTPUT_LOGIC.'
    license = 'gplv2+'
    inputs = ['logic']
    outputs = []
    tags = ['Util']
    channels = (
        {'id': 'data', 'name': 'Data', 'desc': 'Data line', 'idn': 'dec_logic_out_chan_data'},
    )
    logic_output_channels = (
        {'id': 'copy', 'name': 'Copy', 'desc': 'The input level'},
    )
    annotations = (
        ('edge', 'Edge'),
    )

    def reset(self):
        pass

    def start(self):
        self.out_ann = self.register(srd.OUTPUT_ANN)
        self.out_logic = self.register(srd.OUTPUT_LOGIC)

    def decode(self):
        while True:
            (level,) = self.wait({0: 'e'})
            self.put(self.samplenum, self.samplenum + 1, self.out_logic, [0, bytes([level])])
            self.put(self.samplenum, self.samplenum, self.out_ann, [0, ['edge']])

'''
Test decoder: a wait() whose 'skip' count is negative must fail with a
SystemError, also when the type attribute cache misses.
'''

from .pd import Decoder

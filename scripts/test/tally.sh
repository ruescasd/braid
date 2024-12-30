# SPDX-FileCopyrightText: 2024 Sequent Tech <legal@sequentech.io>
#
# SPDX-License-Identifier: AGPL-3.0-only

# hard coded for trustees = 3
(/usr/bin/time -q -o stats.txt -a -f "%E real\t%M kb\t%P cpu\t%U user\t%S sys" bash -c "cd ./demo/1 && ./run.sh") > log1.txt 2>&1 &
(bash -c "cd ./demo/2 && ./run.sh") > log2.txt 2>&1 &
(bash -c "cd ./demo/3 && ./run.sh") > log3.txt 2>&1 &
wait
